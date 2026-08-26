package com.migo.core.store

import android.content.Context
import androidx.datastore.core.DataStore
import androidx.datastore.preferences.core.MutablePreferences
import androidx.datastore.preferences.core.PreferenceDataStoreFactory
import androidx.datastore.preferences.core.Preferences
import androidx.datastore.preferences.core.booleanPreferencesKey
import androidx.datastore.preferences.core.edit
import androidx.datastore.preferences.core.emptyPreferences
import androidx.datastore.preferences.core.stringPreferencesKey
import androidx.datastore.preferences.preferencesDataStoreFile
import com.migo.core.protocol.BandwidthMode
import java.io.IOException
import kotlinx.coroutines.flow.Flow
import kotlinx.coroutines.flow.catch
import kotlinx.coroutines.flow.first
import kotlinx.coroutines.flow.map

/**
 * The user's preferences: everything they chose that is not secret.
 *
 * Sits deliberately next to [Vault] and is its opposite in every respect. The vault holds private keys
 * under a hardware-held wrapping key and refuses to be read without it; this holds a theme choice and a
 * bandwidth mode in a plain file that Android's automatic backup is welcome to carry to the user's next
 * phone. Nothing here is a credential, and the separation is what keeps that true: a settings store
 * that also held the refresh token would have to be sealed, and then a person could not change their
 * theme before unlocking their keys.
 *
 * # Why DataStore and not SharedPreferences
 *
 * Some of these values feed a protocol frame. [AppSettings.bandwidthMode] goes into HELLO, so a client
 * that lets a user change it and reconnect has to know the write landed before it opens the socket.
 * `SharedPreferences` offers `commit()`, which blocks the calling thread, or `apply()`, which reports
 * nothing -- neither is an awaitable write. DataStore's `edit` is a suspending transaction, which is
 * exactly the shape the call site needs.
 *
 * The other half of the reason is reads. A settings screen wants to re-render when a value changes, and
 * `SharedPreferences` answers that with a listener whose registration a caller has to remember to undo.
 * A [Flow] cancels with the scope that collects it.
 *
 * # One snapshot, not one flow per key
 *
 * Everything is read and written as a whole [AppSettings]. A screen that collected a flow per
 * preference would, during any write that touched two of them, render once with the new value of one
 * and the old value of the other. Nobody would notice on a theme change and everybody would notice on
 * the privacy switches, where a half-applied state is a wrong statement about what the app is doing.
 * A single immutable snapshot cannot tear.
 *
 * # Unknown values fall back rather than fail
 *
 * The enum readers map anything they do not recognise onto the default. A newer build might write a
 * value this one has never heard of, and the user might then downgrade or restore a backup; the choice
 * is between a default and a crash on launch. It is the same forward-compatibility rule the protocol
 * enums follow with [BandwidthMode.fromWire], and it applies for the same reason: a peer that predates
 * a value should keep working, not stop.
 *
 * A read that fails outright -- a truncated file, a filesystem error -- yields defaults too, for a
 * stronger version of the same argument. There is nothing in here whose loss is a security problem, so
 * refusing to start because a preferences file is damaged would trade a recoverable annoyance for an
 * unrecoverable one.
 */
class Settings private constructor(private val store: DataStore<Preferences>) {

    /**
     * The current settings, re-emitted on every change.
     *
     * Collect it for the lifetime of a screen. The first emission is the stored state, or the defaults
     * when nothing has been stored yet, so a collector never has to handle an absent-value case.
     */
    val flow: Flow<AppSettings> = store.data
        .catch { cause ->
            // Only an I/O failure is swallowed. A `CancellationException` is the collector going away
            // and must propagate, and anything else is a bug worth crashing on rather than hiding
            // behind a default that looks like a user choice.
            if (cause is IOException) emit(emptyPreferences()) else throw cause
        }
        .map { it.toAppSettings() }

    /** The settings right now, for a one-shot read such as building the HELLO frame. */
    suspend fun current(): AppSettings = flow.first()

    /**
     * Applies a change and returns the result.
     *
     * Read-modify-write inside DataStore's own transaction, so two concurrent updates cannot lose one
     * another's field: `transform` runs against whatever is committed at the moment it runs, not
     * against a snapshot the caller read earlier.
     */
    suspend fun update(transform: (AppSettings) -> AppSettings): AppSettings {
        val updated = store.edit { preferences ->
            val next = transform(preferences.toAppSettings())
            next.writeTo(preferences)
        }
        return updated.toAppSettings()
    }

    /**
     * Resets everything to defaults.
     *
     * Part of signing out, alongside [Vault.destroy]. Not the same operation: the vault destroys key
     * material and this only forgets choices, so a caller that wants both has to ask for both.
     */
    suspend fun clear() {
        store.edit { it.clear() }
    }

    companion object {
        /** The preferences file name, under the app's `datastore` directory. */
        private const val STORE_NAME = "migo-settings"

        /**
         * Opens the settings store for this app.
         *
         * Takes the application context rather than whatever was passed, because a `DataStore` outlives
         * any screen and holding an Activity would leak it.
         */
        fun open(context: Context): Settings {
            val app = context.applicationContext
            val store = PreferenceDataStoreFactory.create(
                produceFile = { app.preferencesDataStoreFile(STORE_NAME) },
            )
            return Settings(store)
        }
    }
}

/** How the app follows the system's light and dark setting. */
enum class ThemeChoice {
    /** Follow the system. */
    System,

    /** Always light. */
    Light,

    /** Always dark. */
    Dark,
}

/**
 * When an attachment is fetched without being asked for.
 *
 * A choice with a cost either way: automatic download spends the user's data plan, and manual download
 * means a photo in a chat is a tap and a wait. Metered-only is the compromise most people want and so
 * is the default.
 */
enum class MediaAutoDownload {
    /** Never fetch until the user taps. */
    Never,

    /** Fetch on an unmetered network only. */
    Unmetered,

    /** Always fetch. */
    Always,
}

/**
 * A complete, immutable settings snapshot.
 *
 * Every field has a default that is correct for a fresh install, so the defaults are also what a caller
 * gets when nothing has been written and when a read fails. The three privacy switches default to on
 * because a messaging app whose read receipts and typing indicators silently did not work would read as
 * broken rather than private, and each is individually disableable for the person who wants that.
 */
data class AppSettings(
    /**
     * The server this account belongs to, e.g. `https://api.migo.example`.
     *
     * Empty until the user has chosen one. Kept here rather than compiled in so one build can serve a
     * self-hosted deployment; the value is also in [SavedSession], and that copy is the authoritative
     * one for a signed-in device -- this one is what a sign-in screen pre-fills.
     */
    val serverUrl: String = "",

    /**
     * The language tag sent in HELLO, for server-composed strings.
     *
     * Empty means "use the system locale", which is what a caller resolves before building the frame.
     * Storing the empty string rather than the resolved tag is what makes the setting keep tracking the
     * system when the user changes their phone's language.
     */
    val locale: String = "",

    /** How much bandwidth the server may spend on this session. Announced in HELLO. */
    val bandwidthMode: BandwidthMode = BandwidthMode.Auto,

    /** Light, dark, or follow the system. */
    val theme: ThemeChoice = ThemeChoice.System,

    /** Whether to raise a notification at all. */
    val notificationsEnabled: Boolean = true,

    /**
     * Whether a notification may show who sent the message and a preview of it.
     *
     * The server cannot leak either one: what it pushes carries no plaintext (brief section 174), so
     * the shade shows a bare "New message" until this device decrypts locally and rewrites it. This
     * setting is what decides whether that rewrite happens -- which makes it a lock-screen privacy
     * control, not a server-side one.
     */
    val notificationPreview: Boolean = true,

    /** Whether to tell a sender their message was read. */
    val sendReadReceipts: Boolean = true,

    /** Whether to publish typing indicators. */
    val sendTypingIndicators: Boolean = true,

    /** Whether to publish presence, so contacts can see this account as online. */
    val sharePresence: Boolean = true,

    /** When an attachment is fetched without being asked for. */
    val mediaAutoDownload: MediaAutoDownload = MediaAutoDownload.Unmetered,

    /** Whether the first-run flow has been completed, so it is not shown again. */
    val onboardingComplete: Boolean = false,
)

// The preference keys. Private to this file: a key is a storage detail, and anything outside that could
// name one could also write a value the readers below do not expect.
private val KEY_SERVER_URL = stringPreferencesKey("server_url")
private val KEY_LOCALE = stringPreferencesKey("locale")
private val KEY_BANDWIDTH_MODE = stringPreferencesKey("bandwidth_mode")
private val KEY_THEME = stringPreferencesKey("theme")
private val KEY_NOTIFICATIONS_ENABLED = booleanPreferencesKey("notifications_enabled")
private val KEY_NOTIFICATION_PREVIEW = booleanPreferencesKey("notification_preview")
private val KEY_SEND_READ_RECEIPTS = booleanPreferencesKey("send_read_receipts")
private val KEY_SEND_TYPING = booleanPreferencesKey("send_typing_indicators")
private val KEY_SHARE_PRESENCE = booleanPreferencesKey("share_presence")
private val KEY_MEDIA_AUTO_DOWNLOAD = stringPreferencesKey("media_auto_download")
private val KEY_ONBOARDING_COMPLETE = booleanPreferencesKey("onboarding_complete")

/**
 * Reads a snapshot, substituting the default for anything absent or unrecognised.
 *
 * The defaults come from [AppSettings]' own constructor rather than being repeated here, so there is one
 * place a default is written down and no way for the two to disagree.
 */
private fun Preferences.toAppSettings(): AppSettings {
    val defaults = AppSettings()
    return AppSettings(
        serverUrl = this[KEY_SERVER_URL] ?: defaults.serverUrl,
        locale = this[KEY_LOCALE] ?: defaults.locale,
        bandwidthMode = readBandwidthMode(this[KEY_BANDWIDTH_MODE], defaults.bandwidthMode),
        theme = readEnum(this[KEY_THEME], ThemeChoice.entries, defaults.theme),
        notificationsEnabled = this[KEY_NOTIFICATIONS_ENABLED] ?: defaults.notificationsEnabled,
        notificationPreview = this[KEY_NOTIFICATION_PREVIEW] ?: defaults.notificationPreview,
        sendReadReceipts = this[KEY_SEND_READ_RECEIPTS] ?: defaults.sendReadReceipts,
        sendTypingIndicators = this[KEY_SEND_TYPING] ?: defaults.sendTypingIndicators,
        sharePresence = this[KEY_SHARE_PRESENCE] ?: defaults.sharePresence,
        mediaAutoDownload = readEnum(
            this[KEY_MEDIA_AUTO_DOWNLOAD],
            MediaAutoDownload.entries,
            defaults.mediaAutoDownload,
        ),
        onboardingComplete = this[KEY_ONBOARDING_COMPLETE] ?: defaults.onboardingComplete,
    )
}

/** Writes a snapshot in full, so a field removed from the snapshot cannot survive in the file. */
private fun AppSettings.writeTo(preferences: MutablePreferences) {
    preferences[KEY_SERVER_URL] = serverUrl
    preferences[KEY_LOCALE] = locale
    preferences[KEY_BANDWIDTH_MODE] = bandwidthMode.name
    preferences[KEY_THEME] = theme.name
    preferences[KEY_NOTIFICATIONS_ENABLED] = notificationsEnabled
    preferences[KEY_NOTIFICATION_PREVIEW] = notificationPreview
    preferences[KEY_SEND_READ_RECEIPTS] = sendReadReceipts
    preferences[KEY_SEND_TYPING] = sendTypingIndicators
    preferences[KEY_SHARE_PRESENCE] = sharePresence
    preferences[KEY_MEDIA_AUTO_DOWNLOAD] = mediaAutoDownload.name
    preferences[KEY_ONBOARDING_COMPLETE] = onboardingComplete
}

/**
 * Resolves an enum by name, falling back to [fallback].
 *
 * By name and not by ordinal. An ordinal is a position in a source file, so inserting a variant would
 * silently reinterpret every stored value -- the kind of change a reviewer cannot see. A name survives
 * reordering, and the only thing that breaks it is a rename, which is a change to a stored format and
 * should be treated as one.
 */
private fun <E : Enum<E>> readEnum(stored: String?, values: List<E>, fallback: E): E {
    if (stored == null) return fallback
    return values.firstOrNull { it.name == stored } ?: fallback
}

/**
 * Resolves a stored bandwidth mode, and never yields [BandwidthMode.Unknown].
 *
 * `Unknown` exists so a frame carrying a discriminant this build predates still decodes. It is not
 * something a person can choose, and putting it in HELLO would announce a mode the server has no
 * meaning for. So a file that somehow contains it reads as the default, the same as any other value
 * this build does not accept.
 */
private fun readBandwidthMode(stored: String?, fallback: BandwidthMode): BandwidthMode {
    val resolved = readEnum(stored, BandwidthMode.entries, fallback)
    return if (resolved == BandwidthMode.Unknown) fallback else resolved
}
