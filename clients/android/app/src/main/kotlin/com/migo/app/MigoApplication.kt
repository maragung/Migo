package com.migo.app

import android.app.Application
import kotlinx.coroutines.CoroutineScope
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.SupervisorJob

/**
 * The application object, which exists for one reason: a scope that outlives every screen.
 *
 * Closing the gateway socket is the last thing this app does, and it happens exactly when the last
 * screen has gone -- which is after `viewModelScope` has been cancelled. A coroutine launched there
 * would never run, so the socket would be left for the server to time out. This scope is the place
 * that work can still be started from.
 *
 * [SupervisorJob] so one failed shutdown does not cancel the rest, and [Dispatchers.IO] because
 * everything launched here is a socket or a file.
 */
class MigoApplication : Application() {
    /**
     * Lives as long as the process and is never cancelled.
     *
     * Deliberately not cancelled in `onTerminate`: Android does not call it on real devices, so code
     * there is code that never runs, and the process ending takes the scope with it anyway.
     */
    val scope: CoroutineScope = CoroutineScope(SupervisorJob() + Dispatchers.IO)
}
