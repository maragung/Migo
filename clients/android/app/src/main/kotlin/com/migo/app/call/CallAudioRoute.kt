package com.migo.app.call

import android.content.Context
import android.media.AudioDeviceCallback
import android.media.AudioDeviceInfo
import android.media.AudioManager
import android.os.Build
import android.os.Handler
import android.os.Looper
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.flow.StateFlow
import kotlinx.coroutines.flow.asStateFlow

/**
 * One route this phone can play a call through, named the way the phone names it.
 *
 * The id is the platform's own device id and not an index into the list: the list is re-read
 * whenever the phone reports a change, and an index would move under the user's finger when a
 * headset is unplugged mid-call.
 */
data class AudioOutput(val id: Int, val label: String)

/**
 * Where a call is played: the routes this phone offers one, and the one it is on.
 *
 * This layer was the one-to-one call's before it was anyone else's, and it lives on its own now
 * because the group call needs exactly the same thing. The two calls are two planes and two
 * managers, but a route is a property of *the phone carrying a call* and not of either plane: the
 * same devices are offered, the same platform call moves the call between them, the same Android
 * 12 gate decides whether the question can be asked at all, and the same product rule hides a menu
 * of one. A second copy inside [GroupCallManager] would be a second set of answers to one
 * question, free to drift from the first -- so each manager holds one of these for the length of
 * its own call and neither reaches into the other's.
 *
 * What the caller keeps is the *when*: this class knows how to watch and how to route, not when a
 * call exists, so [start] and [stop] are the caller's to place. The one-to-one call starts the
 * watch where it connects and stops it in `teardownMedia`, the single place every ending passes
 * through; the group call starts it where its seat goes live and stops it beside `media.stop()`.
 */
internal class CallAudioRoute(context: Context) {
    private val audioManager =
        context.getSystemService(Context.AUDIO_SERVICE) as AudioManager

    private val _outputs = MutableStateFlow<List<AudioOutput>>(emptyList())

    /**
     * The routes this phone offers the call right now, in the order the platform lists them.
     *
     * Empty on a phone older than Android 12, and empty is the honest answer there rather than a
     * short list: the API that predates it can only toggle the speaker, which is one route and not
     * the phone's list, so those devices get no menu at all instead of a menu that cannot move the
     * call. The call screen draws the control only when there is more than one entry, the same rule
     * the web build keeps for a platform that reports a single microphone.
     */
    val outputs: StateFlow<List<AudioOutput>> = _outputs.asStateFlow()

    private val _chosenOutputId = MutableStateFlow<Int?>(null)

    /**
     * The route the call is playing through, or null while the phone is choosing for itself.
     *
     * Null is a real answer and not a missing one: a call the app has never routed anywhere is a
     * call the platform is routing, and the menu shows no tick rather than ticking a device it
     * merely guessed at.
     */
    val chosenOutputId: StateFlow<Int?> = _chosenOutputId.asStateFlow()

    /**
     * The platform's own word on the routes.
     *
     * A headset plugged in mid-call is a route that appears and one unplugged is a route that
     * goes, so a menu that read the list once would offer a device the phone no longer has --
     * which is why the list is re-read from the phone's report rather than from the clock. The
     * report is honoured only while the watch stands ([watching]): an unregister cannot recall a
     * callback the main looper has already queued, and a re-read that landed after [stop] would
     * otherwise refill the list for a call that is over.
     */
    private val outputWatcher = object : AudioDeviceCallback() {
        override fun onAudioDevicesAdded(addedDevices: Array<out AudioDeviceInfo>?) {
            if (watching) refresh()
        }

        override fun onAudioDevicesRemoved(removedDevices: Array<out AudioDeviceInfo>?) {
            if (watching) refresh()
        }
    }

    /**
     * Re-reads the routes the phone is offering and the one it says the call is on.
     *
     * Every failure here is a phone that will not say, and a phone that will not say leaves the
     * list empty and the menu undrawn rather than a control built on a guess.
     */
    private fun refresh() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            return
        }
        val devices = runCatching {
            audioManager.getDevices(AudioManager.GET_DEVICES_OUTPUTS).filter { it.isCallRoute() }
        }.getOrDefault(emptyList())
        _outputs.value = devices.map { AudioOutput(it.id, it.callRouteLabel()) }
        _chosenOutputId.value = runCatching { audioManager.communicationDevice?.id }.getOrNull()
    }

    /** Whether the platform is currently reporting route changes to [outputWatcher]. */
    private var watching = false

    /**
     * Asks the phone to report route changes for as long as the call lasts, and reads the list once
     * to have something to draw before the first report arrives.
     *
     * Guarded rather than merely registered, because the same callback instance registered twice is
     * a report delivered twice, and the callers reach this from every path that marks a call live
     * -- a roster update in a group call is not a new call. The guard is also what makes the
     * second and later calls free.
     */
    fun start() {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S || watching) {
            return
        }
        audioManager.registerAudioDeviceCallback(outputWatcher, Handler(Looper.getMainLooper()))
        watching = true
        refresh()
    }

    /**
     * Gives the route back to the phone.
     *
     * A phone left pinned to a speaker by a call that ended is a phone whose next notification is
     * loud in a room where nobody asked for it, so the pin is cleared with the call rather than
     * carried into the next one -- and the list is emptied with it, so a menu drawn from the state
     * of a call that is over cannot offer a route that call was using.
     */
    fun stop() {
        if (Build.VERSION.SDK_INT >= Build.VERSION_CODES.S && watching) {
            audioManager.unregisterAudioDeviceCallback(outputWatcher)
            runCatching { audioManager.clearCommunicationDevice() }
        }
        watching = false
        _outputs.value = emptyList()
        _chosenOutputId.value = null
    }

    /**
     * Plays the call through one of the routes the phone listed.
     *
     * False means the phone refused, which is what happens when a Bluetooth route drops between
     * the menu being drawn and the tap landing: the refusal is returned rather than swallowed so
     * the caller can re-read the list instead of leaving the menu showing a choice the call is
     * not on. The route is a property of the call the phone is carrying and not of this object,
     * so the choice does not survive the call -- a later call starts on whatever the phone picks,
     * which is the behaviour a person expects from hanging up and calling back.
     */
    fun choose(deviceId: Int): Boolean {
        if (Build.VERSION.SDK_INT < Build.VERSION_CODES.S) {
            return false
        }
        val device = runCatching {
            audioManager.getDevices(AudioManager.GET_DEVICES_OUTPUTS)
                .firstOrNull { it.id == deviceId }
        }.getOrNull() ?: return false
        val routed = runCatching { audioManager.setCommunicationDevice(device) }
            .getOrDefault(false)
        refresh()
        return routed
    }
}

/**
 * Whether this device is one a call can be played through.
 *
 * The list the platform hands back is every output the phone has, including the ones that carry
 * media and not calls, and offering those in a call's routing menu would be offering a tap that
 * moves nothing. What is left is the routes a call genuinely uses: the earpiece, the speaker,
 * wired headsets, USB and Bluetooth headsets, and the routes that only appear once the phone is
 * in communication mode.
 */
private fun AudioDeviceInfo.isCallRoute(): Boolean = when (type) {
    AudioDeviceInfo.TYPE_BUILTIN_EARPIECE,
    AudioDeviceInfo.TYPE_BUILTIN_SPEAKER,
    AudioDeviceInfo.TYPE_WIRED_HEADSET,
    AudioDeviceInfo.TYPE_WIRED_HEADPHONES,
    AudioDeviceInfo.TYPE_USB_HEADSET,
    AudioDeviceInfo.TYPE_USB_DEVICE,
    AudioDeviceInfo.TYPE_BLUETOOTH_SCO,
    AudioDeviceInfo.TYPE_BLE_HEADSET,
    AudioDeviceInfo.TYPE_BLE_SPEAKER,
    AudioDeviceInfo.TYPE_HEARING_AID,
    -> true

    else -> false
}

/**
 * What the menu calls a route.
 *
 * The product name first, because that is the name the user sees in the phone's own settings and
 * the one that tells two headsets apart; the type's word only when the phone left the name blank,
 * which is common for the built-in routes. A route with neither is still listed, under a word that
 * says what it is rather than as an empty row.
 */
private fun AudioDeviceInfo.callRouteLabel(): String {
    val named = runCatching { productName?.toString().orEmpty() }.getOrDefault("")
    if (named.isNotBlank()) {
        return named
    }
    return when (type) {
        AudioDeviceInfo.TYPE_BUILTIN_EARPIECE -> "Phone"
        AudioDeviceInfo.TYPE_BUILTIN_SPEAKER -> "Speaker"
        AudioDeviceInfo.TYPE_WIRED_HEADSET, AudioDeviceInfo.TYPE_WIRED_HEADPHONES -> "Headset"
        AudioDeviceInfo.TYPE_USB_HEADSET, AudioDeviceInfo.TYPE_USB_DEVICE -> "USB audio"
        AudioDeviceInfo.TYPE_BLUETOOTH_SCO, AudioDeviceInfo.TYPE_BLE_HEADSET -> "Bluetooth"
        AudioDeviceInfo.TYPE_BLE_SPEAKER -> "Bluetooth speaker"
        AudioDeviceInfo.TYPE_HEARING_AID -> "Hearing aid"
        else -> "Audio device"
    }
}
