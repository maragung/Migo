package com.migo.app

import android.app.Application

/**
 * Process-wide entry point.
 *
 * Empty on purpose for now. The SDK's MigoClient and key vault are created per screen
 * through a ViewModel rather than held as a process-global singleton, so there is nothing
 * to construct here yet. The class exists because the manifest names it, and it is the one
 * place a future process-scoped dependency graph would be built exactly once.
 */
class MigoApplication : Application()
