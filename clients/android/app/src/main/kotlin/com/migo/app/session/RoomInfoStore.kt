package com.migo.app.session

import com.migo.core.protocol.RoomKind
import com.migo.core.protocol.RoomRole
import com.migo.core.protocol.RoomSummary
import com.migo.core.wire.Id
import com.migo.core.wire.IdParseResult
import com.migo.core.wire.tryParseId
import java.io.File
import java.util.Properties

/**
 * One followed room, as the shell persists it: both halves of the bridge and the public facts a
 * row and a header draw.
 *
 * The record exists because the server keeps rooms and conversations in separate frames on purpose
 * — a conversation summary for a room carries no room id and no name, and a room listing carries
 * no conversation id — and the one wire moment both are visible together is the join reply. What
 * the shell holds in memory after a join therefore has to be re-derived after a restart, and this
 * is the durable half of that: [roomId] and [conversationId] name the bridge, the rest is the
 * floor the room's own state deltas refresh once the topic is watched again.
 */
data class RoomRecord(
    val roomId: Id,
    val conversationId: Id,
    val name: String,
    val kind: RoomKind = RoomKind.Unknown,
    val publicId: String = "",
    val topic: String? = null,
    val memberCount: Long = 0L,
    val onlineCount: Long = 0L,
    val maxMembers: Long? = null,
    val myRole: RoomRole = RoomRole.Unknown,
) {
    /**
     * The record as the room cache's summary shape, for the paths that read [RoomSummary] — the
     * header's live counts and role, the row's name. A floor by construction: the room's own
     * state deltas refresh it once the topic is watched again.
     */
    fun summary(): RoomSummary = RoomSummary(
        roomId = roomId,
        publicId = publicId,
        kind = kind,
        name = name,
        memberCount = memberCount,
        onlineCount = onlineCount,
        topic = topic,
        myRole = myRole.takeIf { it != RoomRole.Unknown },
        maxMembers = maxMembers,
    )
}

/**
 * Persistence for the room metadata the chat shell keeps beside its conversation list.
 *
 * The same store the web client holds in `clients/web/src/lib/storage/room-info-store.ts`, in this
 * app's own idiom (a properties file in the app's private files, the way [com.migo.app.media.VoiceNoteDrafts]
 * keeps its drafts): a room joined weeks ago would otherwise render as an anonymous "Room" row
 * forever after a restart, because nothing on the wire would ever name it again, and the topics it
 * stopped watching would never be re-asked.
 *
 * The record is scoped to the account. Room names and topics are public facts, but they are also
 * a map of where *this* account spends its time, and the next account on the same device has no
 * business inheriting it — a stored copy that names a different account is the caller's to
 * discard, not this store's to merge. Nothing here is key material or a credential; the vault owns
 * everything secret.
 */
object RoomInfoStore {

    private const val DIR_NAME = "room-info"
    private const val FILE_NAME = "rooms.properties"

    private const val ACCOUNT = "account"
    private const val COUNT = "count"

    private const val ROOM = "room"
    private const val CONVERSATION = "conversation"
    private const val NAME = "name"
    private const val KIND = "kind"
    private const val PUBLIC = "public"
    private const val TOPIC = "topic"
    private const val MEMBERS = "members"
    private const val ONLINE = "online"
    private const val MAX = "max"
    private const val ROLE = "role"

    /** The persisted record: which account's rooms these are, and the rooms themselves. */
    data class Stored(val accountId: Id, val rooms: List<RoomRecord>)

    /** Projects a join reply and its bridge into the record this store keeps. */
    fun record(summary: RoomSummary, conversationId: Id): RoomRecord = RoomRecord(
        roomId = summary.roomId,
        conversationId = conversationId,
        name = summary.name,
        kind = summary.kind,
        publicId = summary.publicId,
        topic = summary.topic,
        memberCount = summary.memberCount,
        onlineCount = summary.onlineCount,
        maxMembers = summary.maxMembers,
        myRole = summary.myRole ?: RoomRole.Unknown,
    )

    /** Persists the room record set for `accountId`, replacing whatever was held. */
    fun save(baseDir: File, accountId: Id, rooms: List<RoomRecord>) {
        val properties = Properties()
        properties.setProperty(ACCOUNT, accountId.value)
        properties.setProperty(COUNT, rooms.size.toString())
        for ((index, room) in rooms.withIndex()) {
            val prefix = "$ROOM.$index"
            properties.setProperty("$prefix.$ROOM", room.roomId.value)
            properties.setProperty("$prefix.$CONVERSATION", room.conversationId.value)
            properties.setProperty("$prefix.$NAME", room.name)
            properties.setProperty("$prefix.$KIND", room.kind.wire.toString())
            properties.setProperty("$prefix.$PUBLIC", room.publicId)
            room.topic?.let { properties.setProperty("$prefix.$TOPIC", it) }
            properties.setProperty("$prefix.$MEMBERS", room.memberCount.toString())
            properties.setProperty("$prefix.$ONLINE", room.onlineCount.toString())
            room.maxMembers?.let { properties.setProperty("$prefix.$MAX", it.toString()) }
            if (room.myRole != RoomRole.Unknown) {
                properties.setProperty("$prefix.$ROLE", room.myRole.wire.toString())
            }
        }
        File(dir(baseDir), FILE_NAME).writer().use { properties.store(it, null) }
    }

    /**
     * Reads the persisted record, or null on a first run. An unreadable file reads as null rather
     * than throwing: the session simply starts without remembered rooms, the same floor the web
     * store's failure mode leaves. A per-room entry that does not parse is dropped while the rest
     * survive — one corrupt line must not cost every room.
     */
    fun load(baseDir: File): Stored? {
        val file = File(dir(baseDir), FILE_NAME)
        if (!file.isFile) return null
        val properties = Properties()
        return try {
            file.reader().use { properties.load(it) }
            val account = idOf(properties.getProperty(ACCOUNT)) ?: return null
            val count = properties.getProperty(COUNT)?.toIntOrNull() ?: return null
            val rooms = ArrayList<RoomRecord>(count)
            for (index in 0 until count) {
                recordAt(properties, index)?.let { rooms.add(it) }
            }
            Stored(account, rooms)
        } catch (_: Exception) {
            null
        }
    }

    /** Removes the persisted record (a different account's copy must not survive a sign-out). */
    fun clear(baseDir: File) {
        File(dir(baseDir), FILE_NAME).delete()
    }

    /** The store's directory under [baseDir]; created on first use. */
    private fun dir(baseDir: File): File = File(baseDir, DIR_NAME).apply { mkdirs() }

    /** One room's entry, or null when any load-bearing field is missing or malformed. */
    private fun recordAt(properties: Properties, index: Int): RoomRecord? {
        val prefix = "$ROOM.$index"
        val room = idOf(properties.getProperty("$prefix.$ROOM")) ?: return null
        val conversation = idOf(properties.getProperty("$prefix.$CONVERSATION")) ?: return null
        val name = properties.getProperty("$prefix.$NAME") ?: return null
        return RoomRecord(
            roomId = room,
            conversationId = conversation,
            name = name,
            kind = properties.getProperty("$prefix.$KIND")?.let { value ->
                value.toLongOrNull()?.let(RoomKind::fromWire)
            } ?: RoomKind.Unknown,
            publicId = properties.getProperty("$prefix.$PUBLIC") ?: "",
            topic = properties.getProperty("$prefix.$TOPIC"),
            memberCount = properties.getProperty("$prefix.$MEMBERS")?.toLongOrNull() ?: 0L,
            onlineCount = properties.getProperty("$prefix.$ONLINE")?.toLongOrNull() ?: 0L,
            maxMembers = properties.getProperty("$prefix.$MAX")?.toLongOrNull(),
            myRole = properties.getProperty("$prefix.$ROLE")?.let { value ->
                value.toLongOrNull()?.let(RoomRole::fromWire)
            } ?: RoomRole.Unknown,
        )
    }

    /** Parses a persisted id leniently, or null when the text is not one. */
    private fun idOf(text: String?): Id? = when (text) {
        null -> null
        else -> when (val result = tryParseId(text)) {
            is IdParseResult.Ok -> result.id
            is IdParseResult.Fail -> null
        }
    }
}
