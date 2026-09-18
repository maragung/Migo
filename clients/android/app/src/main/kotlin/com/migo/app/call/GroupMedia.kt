package com.migo.app.call

import com.migo.core.domain.GroupCallSeat
import com.migo.core.protocol.TurnServer
import com.migo.core.wire.Id
import org.webrtc.PeerConnection

/**
 * The group call's media plane, as far as it is a decision rather than a device: the product limit
 * on video, the election that keeps two simultaneous joins from both offering, the roster order
 * that decides who dials whom, and the ICE servers a link is built over.
 *
 * A port of the decision half of `clients/web/src/lib/migo/group-media.ts`, kept pure and separate
 * from the plane that uses it for the same reason the quality ladder is: a group call is a mesh, so
 * every seat runs this same arithmetic against every other seat, and two seats that disagreed about
 * who offers or how many streams the call carries would fail to negotiate or quietly exceed the
 * limit. Pure functions are also the only half that can be tested without a microphone.
 *
 * The ladder itself lives in [LinkQuality] and the rung-to-caps mapping in [VideoCaps]; those are
 * already shared with the one-to-one call, because a link is a link whether or not a third person
 * is on the other end of it.
 */

/** How many active video streams a call carries before the next is refused; the product limit. */
const val MAX_ACTIVE_VIDEO_STREAMS: Int = 8

/** How long one link's gathered candidates linger before one relay carries them (section 165). */
const val DEFAULT_ICE_LINGER_MS: Long = 250L

/** The public STUN fallback every peer connection carries, as the one-to-one plane's does. */
val GROUP_STUN_FALLBACK: PeerConnection.IceServer =
    PeerConnection.IceServer.builder("stun:stun.l.google.com:19302").createIceServer()

/**
 * Whether this device may publish video: it wants to, and fewer than the product limit's worth of
 * video streams would flow with its own added.
 *
 * In the mesh each seat enforces the cap from what it can see, and what it can see at join time is
 * the roster -- the wire carries no media kind -- so the count is the remote seats, which is an
 * upper bound on the video publishers among them. The bound is therefore conservative where the
 * relay core's would be exact: a seat joining a roster of eight refuses its own video even if only
 * three of the eight publish. It is honest in the way that matters, because a call never carries
 * more active video streams than the limit, and a refused seat stays in the call as audio -- the
 * stream is refused, never the participant.
 */
fun videoAdmitted(remoteSeats: Int, wantsVideo: Boolean): Boolean =
    wantsVideo && remoteSeats < MAX_ACTIVE_VIDEO_STREAMS

/**
 * Who keeps their offer when both sides dialed at once.
 *
 * The projections of two concurrent joins can disagree for a moment, and both seats then offer to
 * each other. The lexicographically smaller device id wins, so exactly one side of the glare
 * computes true and rolls back to answer -- and because both sides read the same two ids, they
 * cannot both compute true or both compute false.
 *
 * Compared as text rather than as [Id], which is a value class over a String and carries no
 * ordering of its own; the web build compares the same text the same way.
 */
fun iKeepMyOffer(myDevice: Id, peerDevice: Id): Boolean = myDevice.value < peerDevice.value

/**
 * Whether this device dials a seat, or waits to be dialed.
 *
 * The roster's join order is the whole rule: a seat dials the seats that were already there and
 * waits for the ones that arrive after it. That gives each pair exactly one dialer without either
 * side having to ask, and it is why the order the roster arrives in is a fact the plane depends on
 * rather than a display detail.
 *
 * False when this account holds no seat, and false for a peer that is not in the roster -- a device
 * that has left, or one the roster has not caught up with yet. Both mean the same thing here: there
 * is no link to dial, and a plane that dialed anyway would offer to a seat nobody is sitting in.
 */
fun dialsRemote(seats: List<GroupCallSeat>, me: Id, peerDevice: Id): Boolean {
    val peerIndex = seats.indexOfFirst { it.deviceId == peerDevice }
    if (peerIndex < 0) {
        return false
    }
    val myIndex = seats.indexOfFirst { it.userId == me }
    if (myIndex < 0) {
        return false
    }
    return myIndex > peerIndex
}

/**
 * The ICE servers for a group call's peer connections: the TURN relays the join reply carried,
 * then the public STUN fallback.
 *
 * The relays arrive with short-lived credentials minted for the call and are never embedded in the
 * client, which is why this takes them as an argument rather than fetching them the way the
 * one-to-one plane does -- a group call's relays come back on the join reply, and a seat that has
 * joined has them already. An empty relay list still yields the fallback: a link that only needed
 * STUN must not be refused because the relay list was empty.
 */
fun groupIceServers(relays: List<TurnServer>): List<PeerConnection.IceServer> {
    val servers = ArrayList<PeerConnection.IceServer>(relays.size + 1)
    for (relay in relays) {
        val builder = PeerConnection.IceServer.builder(relay.url)
        if (relay.username.isNotEmpty()) {
            builder.setUsername(relay.username)
        }
        if (relay.credential.isNotEmpty()) {
            builder.setPassword(relay.credential)
        }
        servers.add(builder.createIceServer())
    }
    servers.add(GROUP_STUN_FALLBACK)
    return servers
}
