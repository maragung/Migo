package com.migo.app.call

import android.app.Notification
import android.app.NotificationChannel
import android.app.NotificationManager
import android.app.Service
import android.content.Context
import android.content.Intent
import android.content.pm.ServiceInfo
import android.os.Build
import android.os.IBinder

/**
 * The foreground service a screen share cannot exist without.
 *
 * From Android 14 the platform refuses a display projection to an application that has no
 * foreground service of the mediaProjection type running, and the refusal arrives as a
 * SecurityException from the very call that claims the projection -- which the capturer makes when
 * it is initialized, one step after the user has consented. The service therefore has to be up
 * before the capturer is built, and this class exists to be that service and nothing else: it
 * captures no frames, holds no projection and knows nothing about the call, because the capturer
 * owns both of those and it lives with the call's own manager. What the platform asks for is a
 * running service of the declared type, and this is exactly that and no more.
 *
 * The notification it posts is not decoration: a foreground service is a promise to the user that
 * something is running, and on a screen share the something is that their display is being sent to
 * somebody. It says so, and it stays for as long as the service does.
 */
class ScreenShareService : Service() {
    override fun onBind(intent: Intent?): IBinder? = null

    override fun onStartCommand(intent: Intent?, flags: Int, startId: Int): Int {
        val posted = notification()
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.Q) {
            // The type is given here as well as declared in the manifest, because the two are read
            // at different moments: the manifest is what the platform checks when the service is
            // started, and this is what the service itself claims while it runs.
            startForeground(
                NOTIFICATION_ID,
                posted,
                ServiceInfo.FOREGROUND_SERVICE_TYPE_MEDIA_PROJECTION,
            )
        } else {
            startForeground(NOTIFICATION_ID, posted)
        }
        up = true
        // Not sticky: a share belongs to the call that asked for it, and a service restarted by
        // the platform after that call is gone would be a notification with no call behind it.
        return START_NOT_STICKY
    }

    override fun onDestroy() {
        up = false
        super.onDestroy()
    }

    private fun notification(): Notification {
        val manager = getSystemService(Context.NOTIFICATION_SERVICE) as NotificationManager
        // The channel is created on every start rather than guarded by a flag of our own: the
        // platform already answers whether it exists, and a process that was killed between two
        // shares would otherwise have to remember something it has no reason to.
        if (manager.getNotificationChannel(CHANNEL_ID) == null) {
            manager.createNotificationChannel(
                NotificationChannel(
                    CHANNEL_ID,
                    "Screen sharing",
                    NotificationManager.IMPORTANCE_LOW,
                ),
            )
        }
        return Notification.Builder(this, CHANNEL_ID)
            .setContentTitle("Sharing your screen")
            .setContentText("A Migo call is showing your screen.")
            .setSmallIcon(android.R.drawable.ic_menu_share)
            .setOngoing(true)
            .build()
    }

    companion object {
        private const val CHANNEL_ID = "migo-screen-share"
        private const val NOTIFICATION_ID = 4711

        /**
         * Whether this service has reached the point where the platform counts it as running.
         * Written by the service and read by the caller, hence volatile.
         */
        @Volatile
        private var up: Boolean = false

        /**
         * Waits for the service to come up, and answers whether it did.
         *
         * The wait exists because starting a service is a message to the platform and not a
         * function call: the projection is claimed on the line after this one, it is refused
         * outright where the service is not yet running, and the difference between the two is a
         * race that a caller cannot otherwise see. The bound is what keeps a service that never
         * starts -- a platform refusing to start it at all, say -- from hanging the share forever.
         */
        fun awaitUp(timeoutMs: Long): Boolean {
            val deadline = System.currentTimeMillis() + timeoutMs
            while (!up && System.currentTimeMillis() < deadline) {
                Thread.sleep(POLL_MS)
            }
            return up
        }

        private const val POLL_MS = 5L
    }
}
