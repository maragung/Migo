package com.migo.core.domain

/**
 * One microphone a call may record from, named so the choice outlives the phone's own handle on it.
 *
 * The platform identifies a device by an id it assigns when it enumerates: `AudioDeviceInfo.getId()`
 * is valid for as long as that listing is, and a reboot, a replug or a second headset reassigns it.
 * A stored id is therefore a promise the next call cannot keep — it may name nothing, or worse, name
 * a device the person never chose. What does survive is what the device says about itself: its
 * product name, the platform's type code for it, and its address when it reports one. That triple is
 * what the settings store keeps and what [resolveCallInput] matches against the microphones present
 * later.
 *
 * The type is the platform's own code and is kept as one, uninterpreted: this module has no business
 * reading it, and a client that mapped the codes into a numbering of its own would have to be
 * changed every time the platform added a device kind.
 */
data class CallInputDevice(
    /** What the device calls itself, or empty for one that will not say. */
    val name: String,
    /** The platform's device type code, kept exactly as the platform wrote it. */
    val type: Int,
    /** The device's own address, or empty for one that has none. */
    val address: String,
)

/**
 * What a stored microphone choice amounts to against the microphones the phone has right now.
 *
 * Three answers rather than two, because "nothing was chosen" and "what was chosen is not here" are
 * different states that owe the user different sentences: the first is the phone choosing, which is
 * what every call did before this setting existed, and the second is a choice that cannot be
 * honoured and has to say so rather than quietly doing something else.
 */
sealed interface CallInputResolution {
    /** No choice was ever made: the phone picks, as it did before the setting existed. */
    data object SystemDefault : CallInputResolution

    /** The chosen microphone is present, at [index] in the list it was resolved against. */
    data class Resolved(val index: Int) : CallInputResolution

    /** A choice was made and the microphone is not here — unplugged, or replaced. */
    data class Missing(val saved: CallInputDevice) : CallInputResolution
}

/**
 * Finds the microphone a stored choice names among [live], or says why it cannot.
 *
 * The address is tried first because it is the field a device is most obliged to keep: a USB or
 * Bluetooth device that reports one reports the same one every time it is connected, while a product
 * name is free-form text the platform hands over as it likes. The name is the fallback for the
 * devices that report no address — the built-in microphone among them — and it is only ever used for
 * a device that *has* a name *and* is the only device of its kind wearing it: two microphones the
 * phone will not name are two this cannot tell apart, and a guess between them would record from one
 * device while the pane ticks the other, which is the silent wrongness the setting exists to avoid.
 * Both paths require the type to match too, so a headset that reuses the built-in microphone's name
 * is not mistaken for it.
 *
 * An address identifies a device outright — two devices cannot report the same one — so the first
 * path takes its match as it finds it; only the name path has to prove it is unambiguous.
 */
fun resolveCallInput(saved: CallInputDevice?, live: List<CallInputDevice>): CallInputResolution {
    if (saved == null) return CallInputResolution.SystemDefault
    if (saved.address.isNotEmpty()) {
        val index = live.indexOfFirst { it.address == saved.address && it.type == saved.type }
        if (index >= 0) return CallInputResolution.Resolved(index)
    }
    if (saved.name.isNotEmpty()) {
        val named = live.withIndex()
            .filter { it.value.name == saved.name && it.value.type == saved.type }
        if (named.size == 1) return CallInputResolution.Resolved(named.first().index)
    }
    return CallInputResolution.Missing(saved)
}
