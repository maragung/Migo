package com.migo.core.domain

import org.junit.Assert.assertEquals
import org.junit.Test

/**
 * The stored-microphone lookup, pinned on every answer it can give.
 *
 * The cases that matter are the ones where the phone's list and the stored record disagree, because
 * the two failures this function exists to prevent are both silent: a choice that resolves onto the
 * *wrong* device records the room from a microphone the user did not pick while the pane ticks the
 * one they did, and a choice that resolves onto nothing without saying so is a setting that stopped
 * working with no explanation. So the tests below are as interested in the `Missing` answer and in
 * what is deliberately not matched as they are in the happy path.
 */
class CallInputTest {

    private val builtIn = CallInputDevice(name = "Built-in Mic", type = TYPE_BUILT_IN, address = "")
    private val headset = CallInputDevice(name = "Jabra Evolve2", type = TYPE_USB, address = "usb-1")

    @Test
    fun noStoredChoiceIsThePhoneChoosing() {
        assertEquals(
            CallInputResolution.SystemDefault,
            resolveCallInput(null, listOf(builtIn, headset)),
        )
        // And with nothing to choose from, which is the fresh install on a phone whose platform
        // lists no inputs: still not a missing device, because nothing was ever chosen.
        assertEquals(CallInputResolution.SystemDefault, resolveCallInput(null, emptyList()))
    }

    @Test
    fun aDeviceThatReportsAnAddressIsFoundByIt() {
        // The name is a driver string the platform may rewrite between releases; the address is what
        // the device itself reports, so it is what a reconnect is matched on.
        val renamed = headset.copy(name = "Jabra Evolve2 65")

        assertEquals(
            CallInputResolution.Resolved(1),
            resolveCallInput(headset, listOf(builtIn, renamed)),
        )
    }

    @Test
    fun aDeviceWithNoAddressIsFoundByNameAndType() {
        assertEquals(
            CallInputResolution.Resolved(0),
            resolveCallInput(builtIn, listOf(builtIn, headset)),
        )
    }

    @Test
    fun aDeviceThatIsGoneIsSaidToBeGoneRatherThanGuessedAt() {
        // The stored record is carried out of the answer rather than dropped, because naming the
        // device that is gone is the whole sentence the pane has to write.
        assertEquals(CallInputResolution.Missing(headset), resolveCallInput(headset, listOf(builtIn)))
    }

    @Test
    fun anEmptyListIsAnAnswerAndNotAFailure() {
        // A phone that reports no microphones at all. The stored choice is still the user's, so it is
        // reported missing rather than downgraded to the default, which would erase it from the pane.
        assertEquals(
            CallInputResolution.Missing(headset),
            resolveCallInput(headset, emptyList()),
        )
    }

    @Test
    fun twoDevicesThePhoneWillNotNameAreNeverGuessedBetween() {
        // The built-in microphone on some phones reports neither a name nor an address, so there is
        // nothing in the record to look for: the choice is reported missing rather than resolved onto
        // whichever device the platform happened to list first.
        val anonymous = CallInputDevice(name = "", type = TYPE_BUILT_IN, address = "")

        assertEquals(
            CallInputResolution.Missing(anonymous),
            resolveCallInput(anonymous, listOf(anonymous, anonymous.copy(type = TYPE_LINE_IN))),
        )
    }

    @Test
    fun twoDevicesWearingTheSameNameAreNotChosenBetween() {
        // Two identical headsets, neither reporting an address: the name matches both, and a first
        // match would silently record from one while the pane ticked the other.
        val twin = CallInputDevice(name = "Jabra Evolve2", type = TYPE_USB, address = "")
        val alsoTwin = twin.copy()

        assertEquals(
            CallInputResolution.Missing(twin),
            resolveCallInput(twin, listOf(twin, alsoTwin)),
        )
    }

    @Test
    fun theOnlyDeviceWearingTheNameIsResolved() {
        // The other side of the same rule: one match is not an ambiguity, so a headset that reports a
        // name and no address is usable.
        val twin = CallInputDevice(name = "Jabra Evolve2", type = TYPE_USB, address = "")

        assertEquals(
            CallInputResolution.Resolved(1),
            resolveCallInput(twin, listOf(builtIn, twin)),
        )
    }

    @Test
    fun aNameMatchStillRequiresTheTypeToAgree() {
        // A device kind that reuses another kind's name is not that other device.
        val impostor = CallInputDevice(name = builtIn.name, type = TYPE_LINE_IN, address = "")

        assertEquals(
            CallInputResolution.Missing(builtIn),
            resolveCallInput(builtIn, listOf(impostor)),
        )
    }

    @Test
    fun theAddressIsPreferredOverTheName() {
        // Both fields name a device and they name different ones: the headset's name has been taken
        // by a second device of the same kind while the headset itself is still connected under a new
        // name. The address is the stronger identity, so it decides.
        val detached = headset.copy(name = "Studio monitor", address = "usb-2")
        val renamed = headset.copy(name = "Renamed headset")

        assertEquals(
            CallInputResolution.Resolved(1),
            resolveCallInput(headset, listOf(detached, renamed)),
        )
    }

    private companion object {
        /** Two platform type codes, used as opaque values: this module never reads them. */
        const val TYPE_BUILT_IN = 15
        const val TYPE_USB = 11
        const val TYPE_LINE_IN = 5
    }
}
