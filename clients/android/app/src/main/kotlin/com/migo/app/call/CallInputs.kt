package com.migo.app.call

import android.media.AudioDeviceInfo
import android.media.AudioManager
import com.migo.core.domain.CallInputDevice
import com.migo.core.domain.CallInputResolution
import com.migo.core.domain.resolveCallInput
import org.webrtc.audio.JavaAudioDeviceModule

/**
 * One microphone as a picker shows it: the platform's handle for *this* listing, and the identity
 * that outlives it.
 *
 * The two are kept side by side on purpose. [id] is what the platform takes back when a device is
 * chosen — it is the only name `setPreferredInputDevice` answers to — and it is worthless the moment
 * the listing is re-read, which is why [device] is what gets stored.
 */
data class CallInput(val id: Int, val label: String, val device: CallInputDevice)

/**
 * Reads the microphones the phone is offering right now.
 *
 * An empty list is a legitimate answer, not a failure: a phone with no microphone the platform will
 * enumerate — and a phone that refuses the call outright — both land here, and the caller decides
 * what to draw from it. Every failure is treated the same way, because there is nothing a settings
 * pane or a call can do about a platform that will not list its own devices, and a crash there would
 * cost the user a call over a menu.
 *
 * Inputs only, filtered on `isSource` as well as the request: a USB headset is a source and a sink at
 * once and appears in both listings, and the sink half of it is not something a call can record from.
 */
fun callInputsOf(audioManager: AudioManager): List<CallInput> =
    runCatching {
        audioManager.getDevices(AudioManager.GET_DEVICES_INPUTS)
            .filter { it.isSource }
            .map { info ->
                CallInput(
                    id = info.id,
                    label = info.microphoneLabel(),
                    device = info.toCallInputDevice(),
                )
            }
    }.getOrDefault(emptyList())

/** What about this device survives the listing: its own name, its type, and its address if it has one. */
fun AudioDeviceInfo.toCallInputDevice(): CallInputDevice = CallInputDevice(
    name = productName?.toString().orEmpty().trim(),
    type = type,
    address = address.orEmpty(),
)

/**
 * What to call this microphone on screen.
 *
 * The device's own name first, and a word for its kind only when it will not give one. The output
 * picker's labels are pure words because a route is a kind of thing — "Speaker" is the whole truth
 * about a phone's loudspeaker — while which *microphone* a call records from is the entire point of
 * this list, and a phone with a built-in microphone, a headset and a USB interface would be three
 * rows reading "Headset" or "Microphone" with nothing to choose between them. The platform's own
 * picker shows product names for the same reason.
 */
fun AudioDeviceInfo.microphoneLabel(): String {
    val named = productName?.toString().orEmpty().trim()
    if (named.isNotEmpty()) return named
    return when (type) {
        AudioDeviceInfo.TYPE_BUILTIN_MIC -> "Phone mic"
        AudioDeviceInfo.TYPE_WIRED_HEADSET -> "Headset"
        AudioDeviceInfo.TYPE_BLUETOOTH_SCO, AudioDeviceInfo.TYPE_BLE_HEADSET -> "Bluetooth"
        AudioDeviceInfo.TYPE_USB_DEVICE, AudioDeviceInfo.TYPE_USB_HEADSET -> "USB audio"
        AudioDeviceInfo.TYPE_HEARING_AID -> "Hearing aid"
        AudioDeviceInfo.TYPE_FM_TUNER,
        AudioDeviceInfo.TYPE_LINE_ANALOG,
        AudioDeviceInfo.TYPE_LINE_DIGITAL,
        -> "Line in"
        else -> "Microphone"
    }
}

/**
 * The microphone calls record from, as process-wide state.
 *
 * Process-wide rather than a field of either engine because there are two of them: a one-to-one call
 * is [CallManager]'s engine and a group call is [GroupMediaPlane]'s, each builds its own
 * `JavaAudioDeviceModule`, and a device is never in both at once — so the two never run together,
 * and a preference the user sets is a preference both have to honour. Holding it here is what lets
 * each engine apply it in one line where it opens its audio, without either engine growing a
 * constructor parameter that a future call site has to remember to thread.
 *
 * This is the *memory* of the choice; the settings store is the durable copy, and the view model
 * writes both. Nothing is applied until an engine asks, and a device that is not connected resolves
 * to nothing, which puts the call back on the phone's own choice — the same call a fresh install
 * makes.
 */
internal object PreferredCallInput {

    @Volatile
    private var saved: CallInputDevice? = null

    /** Replaces the remembered choice, where null means "the phone decides". */
    fun remember(device: CallInputDevice?) {
        saved = device
    }

    /**
     * Points [module]'s capture at the remembered microphone, if the phone still has it.
     *
     * Called where a call's audio is opened, before the capture is: WebRTC reads the preference when
     * it starts recording, so a device named before the source exists is applied to the recording
     * that follows, and one named while a call runs is applied to the recording already going. Both
     * paths end in the same `AudioRecord`, which is why this is safe to call from either side.
     *
     * The resolution is done against a fresh listing rather than a cached one, because the moment a
     * call starts is the first moment the answer matters and a headset that was plugged in since the
     * settings pane was drawn must be found.
     */
    fun applyTo(module: JavaAudioDeviceModule, audioManager: AudioManager) {
        val live = callInputsOf(audioManager)
        val chosen = when (val resolution = resolveCallInput(saved, live.map { it.device })) {
            is CallInputResolution.Resolved -> live[resolution.index].id
            else -> null
        }
        module.setPreferredInputDevice(deviceInfoOf(audioManager, chosen))
    }
}

/**
 * The platform's own object for one of the microphones [callInputsOf] listed, or null.
 *
 * A second listing rather than a cached one: an [AudioDeviceInfo] is a snapshot of the device at the
 * moment it was enumerated, and the platform's setter is documented to want a device it currently
 * knows about. Re-reading is one call at the start of a call, against a listing of at most a handful
 * of devices.
 */
private fun deviceInfoOf(audioManager: AudioManager, id: Int?): AudioDeviceInfo? {
    if (id == null) return null
    return runCatching {
        audioManager.getDevices(AudioManager.GET_DEVICES_INPUTS).firstOrNull { it.id == id }
    }.getOrNull()
}
