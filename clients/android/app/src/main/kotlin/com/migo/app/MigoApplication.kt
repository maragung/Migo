package com.migo.app

import android.app.Application
import androidx.lifecycle.ViewModel
import androidx.lifecycle.ViewModelProvider
import androidx.lifecycle.ViewModelStore
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob

/**
 * The application object, which exists for two reasons: a scope that outlives every screen, and
 * the one session view model every screen shares.
 *
 * # The scope
 *
 * Closing the gateway socket is the last thing this app does, and it happens exactly when the last
 * screen has gone -- which is after `viewModelScope` has been cancelled. A coroutine launched there
 * would never run, so the socket would be left for the server to time out. This scope is the place
 * that work can still be started from.
 *
 * [SupervisorJob] so one failed shutdown does not cancel the rest, and [Dispatchers.IO] because
 * everything launched here is a socket or a file.
 *
 * # The view model
 *
 * The session view model lives here rather than in any activity's own store, because chat-list
 * mode reads it from two activities at once: the main activity's list and the chat screen it
 * stacked on top. Two activity-scoped instances would be two sessions -- two sockets, two sets of
 * listeners, two copies of every message -- and the only way to keep that from happening is for
 * there to be one instance that both reach for. It is still a [ViewModelStore] citizen rather than
 * a bare field so the clear path stays the framework's own: [releaseSession] is the last activity
 * finishing, and the store's clear is what runs the view model's own teardown (the socket close
 * above) exactly when it always ran.
 */
class MigoApplication : Application() {
    /**
     * Lives as long as the process and is never cancelled.
     *
     * Deliberately not cancelled in `onTerminate`: Android does not call it on real devices, so code
     * there is code that never runs, and the process ending takes the scope with it anyway.
     */
    val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)

    /** The one session view model store, cleared by the last activity to finish. */
    private val sessionStore = ViewModelStore()

    private val sessionFactory = object : ViewModelProvider.Factory {
        @Suppress("UNCHECKED_CAST")
        override fun <T : ViewModel> create(modelClass: Class<T>): T =
            AppViewModel(this@MigoApplication) as T
    }

    /**
     * The process's one [AppViewModel], created on first reach and shared by every activity.
     *
     * Lazy so nothing -- not the socket, not the session resume -- happens until a screen actually
     * asks for the session, which keeps the application object a home for the view model rather
     * than an eager starter of it.
     */
    val appViewModel: AppViewModel by lazy {
        ViewModelProvider(sessionStore, sessionFactory)[AppViewModel::class.java]
    }

    /**
     * Lets the session view model go: the last activity is finishing, so the app is leaving, and
     * this is the moment its store runs the view model's own close-the-socket teardown.
     *
     * A configuration change never calls this -- the activity is being rebuilt, not left, and the
     * store must survive it -- and a stacked chat activity never calls it either, because the main
     * activity below it is the one whose finish means the whole task is going away.
     */
    fun releaseSession() {
        sessionStore.clear()
    }
}
