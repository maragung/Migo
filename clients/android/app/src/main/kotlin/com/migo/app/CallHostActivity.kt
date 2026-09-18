package com.migo.app

import android.app.PictureInPictureParams
import android.content.pm.PackageManager
import android.content.res.Configuration
import android.util.Rational
import androidx.activity.ComponentActivity
import androidx.compose.runtime.MutableState
import androidx.compose.runtime.mutableStateOf

/**
 * A window that can draw a call, and can hand it to the system's picture-in-picture window.
 *
 * Both activities that can hold a call extend this rather than each keeping its own copy, because
 * the mechanism is the same on both and the failure mode of two copies is a screen that minimises
 * from one surface and not from the other -- the call is one call whichever surface answered it,
 * and a control that works here and not there is a control the user cannot learn.
 *
 * # Why the activity survives the transition
 *
 * Both activities declare the configuration changes this brings in the manifest. That is not a
 * detail of the API but the whole reason the call keeps working: entering the window changes the
 * configuration, and an activity recreated by it would come back with a fresh composition and a
 * call screen the media session is no longer drawing into.
 *
 * # Why the window is asked rather than assumed
 *
 * Picture-in-picture arrived in API 26, which this app's minimum is, so the call itself needs no
 * version check. The feature is nonetheless optional on a device -- a television or a build with
 * the window manager trimmed -- so every path asks [canPictureInPicture] first, and the call screen
 * draws its minimise control only where the answer is yes. A button that does nothing is worse than
 * no button, and a window that opens and immediately closes reads as a crash.
 */
abstract class CallHostActivity : ComponentActivity() {
    /**
     * Whether the system is currently drawing this activity as its picture-in-picture window.
     *
     * State rather than a plain field because the composition reads it: the call screen's shape
     * depends on it, and the change has to recompose the screen rather than wait for the next
     * unrelated state change to redraw it.
     */
    val inPictureInPicture: MutableState<Boolean> = mutableStateOf(false)

    /**
     * Reports whether the system is drawing this activity small, so the call screen can drop
     * everything that was drawn for a full one.
     */
    override fun onPictureInPictureModeChanged(
        isInPictureInPictureMode: Boolean,
        newConfig: Configuration,
    ) {
        super.onPictureInPictureModeChanged(isInPictureInPictureMode, newConfig)
        inPictureInPicture.value = isInPictureInPictureMode
    }

    /** Whether this device can draw a picture-in-picture window at all. */
    fun canPictureInPicture(): Boolean =
        packageManager.hasSystemFeature(PackageManager.FEATURE_PICTURE_IN_PICTURE)

    /**
     * Hands the call to the system's picture-in-picture window, and answers whether it took it.
     *
     * The ratio is the one a landscape camera sends, which is what the remote video is, so the
     * window is filled rather than letterboxed. False means the window did not open -- the feature
     * is absent, or the system refused it because something else is already in one -- and the
     * caller does nothing about that: the full-screen screen is still up and still correct, and a
     * second attempt at a refusal would only be a second refusal.
     */
    fun enterCallPip(): Boolean {
        if (!canPictureInPicture()) {
            return false
        }
        // A preview ratio is the one thing the builder requires before API 31; the system fills
        // the window itself on newer releases, and naming it here keeps every version the same.
        return enterPictureInPictureMode(
            PictureInPictureParams.Builder().setAspectRatio(Rational(16, 9)).build(),
        )
    }
}
