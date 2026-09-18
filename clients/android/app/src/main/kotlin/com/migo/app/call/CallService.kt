package com.migo.app.call

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.PendingIntent
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder
import com.migo.app.AppViewModel
import com.migo.app.MainActivity
import com.migo.app.MigoApplication
import com.migo.core.domain.CallState
import com.migo.core.domain.callStateLabel
import com.migo.core.domain.displayStateOf
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.Job
import kotlinx.coroutines.SupervisorJob
import kotlinx.coroutines.cancel
import kotlinx.coroutines.flow.collectLatest
import kotlinx.coroutines.launch

/**
 * The call's own foreground service: what keeps a conversation alive once the screen it started on
 * is no longer in front, and what gives the call's controls a place to live when there is no call
 * screen to draw them on.
 *
 * # Why it has to exist
 *
 * A call in this app lives in the session view model, and that view model lives in a store the last
 * activity's finish clears. Pressing home does not finish an activity, so a call survives that by
 * itself -- but nothing stops the platform reclaiming a background process, and a microphone that
 * stops because the system wanted memory is a call that dropped for a reason its users cannot see.
 * A foreground service is the platform's own answer to that: it says this process is doing
 * something the user asked for and knows about, and the platform keeps it. The declared type is the
 * microphone, because a call is the microphone running, and that is the fact the platform weighs.
 *
 * # Why the notification carries the controls
 *
 * A foreground service must show a notification, and one that says only "in a call" is the worst of
 * both: it takes the user's attention and gives nothing back. This one carries the two things
 * somebody away from the call screen most needs -- stop being heard, and stop the call -- and it
 * carries them from wherever the notification appears, the lock screen included, which is the only
 * place a person can reach while the phone is in a pocket. Its content intent opens the app,
 * because the notification is also the way back to the call.
 *
 * # Where it starts and stops
 *
 * Started and stopped by the session's view model as the tracked call comes and goes, never by a
 * screen: a call that outlives its screen is exactly the case this exists for, so a screen cannot
 * be what owns it. Stopped the moment the call is over as well as when it is gone, since a
 * notification about a call that has ended is a notification that lies.
 */
class CallService : Service() {
    /**
     * The watcher's own scope rather than the application's: it is cancelled with the service, and
     * the application's scope is not this service's to cancel.
     */
    private val scope = CoroutineScope(SupervisorJob() + Dispatchers.Main.immediate)

    private var watching: Job? = null

    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val model = (application as MigoApplication).appViewModel
        // The actions arrive as intents to this same service, which is the shortest path there is:
        // the platform is already holding this component up, and routing them through a second one
        // would only add a hop that can be killed without the call ever knowing.
        when (intent?.action) {
            ACTION_TOGGLE_MUTE -> model.toggleCallMute()
            ACTION_HANG_UP -> model.hangUpCall()
        }
        val state = model.callState.value
        val call = state.call
        if (call == null || call.state == CallState.Ended) {
            // Nothing to carry: an action that arrived after the call ended, or a start that raced
            // the ending. The service goes rather than lingering as a notification about nothing.
            stopSelf()
            return START_NOT_STICKY
        }
        val posted = notification(model, call, state.muted)
        // The claim is a step the platform can refuse -- a microphone-typed service it decides the
        // app is not entitled to run right now, on the versions that weigh that -- and a refusal
        // raised here would come out of a service callback, which is the one place an exception
        // takes the whole process with it. Swallowed instead: the call is running either way and
        // what is missing without this is the notification, so the service goes quietly rather
        // than taking the conversation down with it.
        val claimed = runCatching {
            if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.R) {
                startForeground(
                    NOTIFICATION_ID,
                    posted,
                    ServiceInfo.FOREGROUND_SERVICE_TYPE_MICROPHONE,
                )
            } else {
                startForeground(NOTIFICATION_ID, posted)
            }
        }.isSuccess
        if (!claimed) {
            stopSelf()
            return START_NOT_STICKY
        }
        watch(model)
        // Not sticky: a service the platform restarted after the call that asked for it is gone
        // would be a notification for a conversation nobody is in.
        return START_NOT_STICKY
    }

    /**
     * Keeps the notification in step with the call, and ends the service when the call does.
     *
     * One watch per service rather than one per start command: every action and every restart
     * arrives here, and a second collector would post every update twice.
     */
    private fun watch(model: AppViewModel) {
        if (watching != null) {
            return
        }
        watching = scope.launch {
            model.callState.collectLatest { state ->
                val call = state.call
                if (call == null || call.state == CallState.Ended) {
                    stopSelf()
                    return@collectLatest
                }
                val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
                manager.notify(NOTIFICATION_ID, notification(model, call, state.muted))
            }
        }
    }

    override fun onDestroy() {
        scope.cancel()
        watching = null
        super.onDestroy()
    }

    /**
     * The call, as the notification states it: whose call it is, what state it is in, and the two
     * controls somebody away from the screen can use.
     */
    private fun notification(model: AppViewModel, call: ActiveCall, muted: Boolean): Notification {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        // Created on every post rather than guarded by a flag of our own: the platform already
        // answers whether it exists, and a process killed between two calls would otherwise have
        // to remember something it has no reason to.
        if (manager.getNotificationChannel(CHANNEL_ID) == null) {
            manager.createNotificationChannel(
                NotificationChannel(CHANNEL_ID, "Calls", NotificationManager.IMPORTANCE_LOW),
            )
        }
        val peer = if (call.isCaller) call.calleeId else call.callerId
        // The same state line the call screen draws, through the same two functions and with the
        // same neutral degraded flag the screen passes (this client pauses no video for a poor
        // link), so the notification and the screen can never disagree about what the call is
        // doing -- a notification that said something the screen does not would be the worse of
        // the two to believe, since it is the one read while the screen is not being looked at.
        val status = callStateLabel(displayStateOf(call.state, degraded = false))
        val open = PendingIntent.getActivity(
            this,
            0,
            Intent(this, MainActivity::class.java),
            PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
        )
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle(model.displayName(peer))
            .setContentText(status)
            .setSmallIcon(android.R.drawable.ic_menu_call)
            .setContentIntent(open)
            .setCategory(Notification.CATEGORY_CALL)
            .setOngoing(true)
            // The notification is rebuilt on every change, and a call that re-alerted on each of
            // them would buzz a pocket every time the state moved.
            .setOnlyAlertOnce(true)
            .addAction(
                android.R.drawable.ic_lock_silent_mode,
                if (muted) "Unmute" else "Mute",
                action(ACTION_TOGGLE_MUTE, 1),
            )
            .addAction(
                android.R.drawable.ic_menu_close_clear_cancel,
                "End call",
                action(ACTION_HANG_UP, 2),
            )
            .build()
    }

    private fun action(name: String, request: Int): PendingIntent = PendingIntent.getService(
        this,
        request,
        Intent(this, CallService::class.java).setAction(name),
        PendingIntent.FLAG_IMMUTABLE or PendingIntent.FLAG_UPDATE_CURRENT,
    )

    companion object {
        private const val CHANNEL_ID = "migo-calls"
        private const val NOTIFICATION_ID = 4712

        /** The action that toggles this device's microphone on the live call. */
        const val ACTION_TOGGLE_MUTE = "com.migo.app.call.TOGGLE_MUTE"

        /** The action that ends the live call. */
        const val ACTION_HANG_UP = "com.migo.app.call.HANG_UP"

        /**
         * Starts the call's foreground service. Called where a call begins, which is a moment the
         * app is by definition in front of the user, because that is where a call is placed or
         * answered -- and the platform refuses a background start, which is why this is not called
         * from anywhere a call could arrive unannounced. A refusal is swallowed rather than
         * raised: the call itself does not depend on the service, only its notification does, and
         * a crash here would take down the very call the refusal was protecting.
         */
        fun start(context: Context) {
            runCatching { context.startForegroundService(Intent(context, CallService::class.java)) }
        }

        /** Stops it. The service also stops itself once the call is over. */
        fun stop(context: Context) {
            runCatching { context.stopService(Intent(context, CallService::class.java)) }
        }
    }
}
