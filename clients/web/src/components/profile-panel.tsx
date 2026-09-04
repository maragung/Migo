'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ChangeEvent, ReactNode } from 'react';

import { PresenceState } from '@migo/sdk';
import type { BadgeWire, ProfileUpdate, ProgressionWire, UserProfile } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { uploadAvatarMedia } from '@/lib/migo/media.js';
import { cacheProfile, refreshProfile } from '@/lib/migo/use-profiles.js';
import type { ResolvedProfile } from '@/lib/migo/use-profiles.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Avatar } from './avatar.js';
import { Icon } from './icons.js';
import { ProgressionCard } from './wallet-panel.js';
import { Spinner } from './spinner.js';

/**
 * The privacy selects' "leave as-is" value.
 *
 * {@link UserProfile} deliberately carries no privacy settings — they are not public data — so the
 * panel cannot pre-select the current one. An empty value means "do not touch", and only an explicit
 * choice joins the save patch; a naive select with a guessed default would silently re-state the
 * setting for a user who only came to fix their display name.
 */
const UNCHANGED = '';

/** The birth-year bounds the panel accepts; anything else is left out of the patch, not sent. */
// The floor is not a statement about living memory — the server holds any i16 year and
// the wire any u32 — it is a typo net: a three-digit year is a slip of the keyboard, and
// a person born before 1900 would be the oldest human who ever lived.
const BIRTH_YEAR_MIN = 1900;
const BIRTH_YEAR_MAX = 2100;

/** The three visibility audiences the protocol defines (0 nobody, 1 friends, 2 everyone). */
const VISIBILITIES: ReadonlyArray<{ value: string; label: string }> = [
  { value: '0', label: 'Nobody' },
  { value: '1', label: 'Friends' },
  { value: '2', label: 'Everyone' },
];

interface PrivacyField {
  key: 'showLastSeen' | 'whoCanMessage' | 'whoCanAdd';
  label: string;
  hint: string;
}

const PRIVACY_FIELDS: ReadonlyArray<PrivacyField> = [
  {
    key: 'showLastSeen',
    label: 'Last seen visible to',
    hint: 'Who can see when you were last online.',
  },
  {
    key: 'whoCanMessage',
    label: 'Who can message me',
    hint: 'Who may start a conversation with you.',
  },
  {
    key: 'whoCanAdd',
    label: 'Who can add me as a friend',
    hint: 'Who may send you a friend request.',
  },
];

/**
 * The Profile tab: view your public profile, edit the display name and bio, and adjust privacy.
 *
 * Saving is patch-shaped on purpose ({@link ProfileDomain.updateProfile} sends only the fields
 * present), so this panel builds the patch from exactly what changed: the two text fields when their
 * content moved, a privacy select only when an audience was explicitly chosen. A user editing their
 * name therefore never re-states a privacy setting they never saw.
 *
 * The same posture covers the searchable switch — the wire's profile carries no current value for
 * it, so the switch joins the patch only once flipped. The birth year used to share that posture;
 * since the wire grew `birthYear` it seeds from the profile and joins the patch exactly like the
 * text fields, when its draft differs from what the server holds. The custom status is a different
 * wire: it is presence, not profile ({@link PresenceDomain.setPresence} carries it), so it is
 * published on its own after the save.
 *
 * Standing — level, XP, badges — arrives from the economy domain and is view-only here; the Gifts
 * tab owns the interactive side of the same facts.
 */
export function ProfilePanel({ onOpenSettings }: { onOpenSettings?: () => void }): ReactNode {
  const { client, accountId } = useMigo();

  const [profile, setProfile] = useState<ResolvedProfile | null>(null);
  const [displayName, setDisplayName] = useState('');
  const [bio, setBio] = useState('');
  const [customStatus, setCustomStatus] = useState('');
  const [birthYear, setBirthYear] = useState('');
  const [searchable, setSearchable] = useState(false);
  const [searchableTouched, setSearchableTouched] = useState(false);
  const [privacy, setPrivacy] = useState<Record<PrivacyField['key'], string>>({
    showLastSeen: UNCHANGED,
    whoCanMessage: UNCHANGED,
    whoCanAdd: UNCHANGED,
  });
  const [progression, setProgression] = useState<ProgressionWire | null>(null);
  const [badges, setBadges] = useState<BadgeWire[] | null>(null);
  const [primed, setPrimed] = useState(false);
  const [busy, setBusy] = useState(false);
  const [avatarBusy, setAvatarBusy] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [saved, setSaved] = useState(false);
  const avatarInputRef = useRef<HTMLInputElement | null>(null);

  useEffect(() => {
    if (!client || !accountId) {
      return;
    }
    let cancelled = false;
    // The load goes through the shared profile cache, so the panel's copy and every other
    // surface (the sidebar's self avatar) are one fact, and the avatar arrives resolved.
    refreshProfile(client, accountId)
      .then((resolved) => {
        if (!cancelled && resolved !== null) {
          setProfile(resolved);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setError('Could not load your profile.');
        }
      });
    // Standing facts degrade quietly: a profile without its level or badges is still a profile.
    void client.economy
      .getProgression(accountId)
      .then((standing) => {
        if (!cancelled) {
          setProgression(standing);
        }
      })
      .catch(() => {});
    void client.economy
      .getBadges(accountId)
      .then((earned) => {
        if (!cancelled) {
          setBadges(earned);
        }
      })
      .catch(() => {});
    return () => {
      cancelled = true;
    };
  }, [client, accountId]);

  // Seed the form once, when the profile first arrives; after that the draft is the user's own.
  useEffect(() => {
    if (primed || profile === null) {
      return;
    }
    setDisplayName(profile.displayName);
    setBio(profile.bio ?? '');
    setCustomStatus(profile.customStatus ?? '');
    setBirthYear(profile.birthYear === undefined ? '' : String(profile.birthYear));
    setPrimed(true);
  }, [profile, primed]);

  const save = useCallback(async (): Promise<void> => {
    if (!client || !profile || busy) {
      return;
    }
    const patch = buildProfilePatch(profile, { displayName, bio }, privacy, {
      birthYear,
      ...(searchableTouched ? { searchable } : {}),
    });
    const statusChanged = customStatus !== (profile.customStatus ?? '');
    if (Object.keys(patch).length === 0 && !statusChanged) {
      return;
    }

    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      if (Object.keys(patch).length > 0) {
        const updated = await client.profile.updateProfile(patch);
        // The reply is the authoritative profile; adopt it through the shared cache so the avatar
        // URL is resolved and every other surface (the sidebar) moves with this save, not after a
        // refetch it has no reason to make.
        const resolved = await cacheProfile(client, updated);
        setProfile(resolved);
        setDisplayName(resolved.displayName);
        setBio(resolved.bio ?? '');
        setPrivacy({
          showLastSeen: UNCHANGED,
          whoCanMessage: UNCHANGED,
          whoCanAdd: UNCHANGED,
        });
        setBirthYear(resolved.birthYear === undefined ? '' : String(resolved.birthYear));
        setSearchableTouched(false);
      }
      if (statusChanged) {
        // The custom status rides the presence wire, not the profile patch: publish it beside
        // the current presence state so saving it does not silently flip the user online.
        const trimmed = customStatus.trim();
        await client.presence.setPresence(
          profile.presence ?? PresenceState.Online,
          trimmed.length > 0 ? { customStatus: trimmed } : {},
        );
      }
      setSaved(true);
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setBusy(false);
    }
  }, [
    client,
    profile,
    busy,
    displayName,
    bio,
    privacy,
    birthYear,
    searchable,
    searchableTouched,
    customStatus,
  ]);

  /**
   * Uploads a picked image as the new avatar, then patches the profile to point at it.
   *
   * The two steps are one action to the user, so the busy state covers both and a failure anywhere
   * surfaces beside the picker that started it. The reply is adopted through the shared cache
   * exactly like {@link save} does — with the new avatar's URL resolved before the panel shows
   * it, so the picture that appears is the one just uploaded, and the sidebar's self avatar
   * follows the same write instead of showing the old picture until some later refetch.
   */
  const changeAvatar = useCallback(
    async (file: File): Promise<void> => {
      if (!client || avatarBusy) {
        return;
      }
      setAvatarBusy(true);
      setError(null);
      setSaved(false);
      try {
        const mediaId = await uploadAvatarMedia(client, file);
        const updated = await client.profile.updateProfile({ avatarMediaId: mediaId });
        const resolved = await cacheProfile(client, updated);
        setProfile(resolved);
        setDisplayName(resolved.displayName);
        setBio(resolved.bio ?? '');
        setSaved(true);
      } catch (cause) {
        setError(friendlyError(cause));
      } finally {
        setAvatarBusy(false);
      }
    },
    [client, avatarBusy],
  );

  function onAvatarChange(event: ChangeEvent<HTMLInputElement>): void {
    const file = event.target.files?.[0];
    // Reset the input so picking the same file again still fires a change event.
    event.target.value = '';
    if (file === undefined) {
      return;
    }
    void changeAvatar(file);
  }

  const dirty =
    profile !== null &&
    (displayName.trim() !== profile.displayName ||
      bio !== (profile.bio ?? '') ||
      customStatus !== (profile.customStatus ?? '') ||
      birthYear !== (profile.birthYear === undefined ? '' : String(profile.birthYear)) ||
      searchableTouched ||
      privacy.showLastSeen !== UNCHANGED ||
      privacy.whoCanMessage !== UNCHANGED ||
      privacy.whoCanAdd !== UNCHANGED);

  if (profile === null && error === null) {
    return (
      <div className="panel">
        <div className="center-fill">
          <Spinner />
        </div>
      </div>
    );
  }

  return (
    <div className="panel">
      <h1 className="panel-title">Profile</h1>

      {profile !== null ? (
        <div className="profile-head">
          <Avatar
            name={profile.displayName}
            id={profile.userId}
            size={56}
            avatarUrl={profile.avatarUrl}
          />
          <div className="profile-id">
            <span className="person-name">{profile.displayName}</span>
            {profile.username ? <span className="person-sub">@{profile.username}</span> : null}
            <span className="person-note">{profile.publicId}</span>
          </div>
          <div className="avatar-actions">
            <input
              ref={avatarInputRef}
              type="file"
              accept="image/*"
              onChange={onAvatarChange}
              hidden
              aria-label="Upload a new avatar"
            />
            <button
              type="button"
              className="btn btn-ghost"
              onClick={() => avatarInputRef.current?.click()}
              disabled={avatarBusy}
            >
              {avatarBusy ? <Spinner /> : 'Change photo'}
            </button>
            {onOpenSettings !== undefined ? (
              <button
                type="button"
                className="btn btn-ghost"
                onClick={onOpenSettings}
                aria-label="Open settings"
              >
                <Icon name="settings" size={16} /> Settings
              </button>
            ) : null}
          </div>
        </div>
      ) : null}

      {error ? <p className="form-error">{error}</p> : null}
      {saved ? <p className="hint">Profile saved.</p> : null}

      {progression !== null || badges !== null ? (
        <fieldset className="panel-section">
          <legend className="panel-heading">Standing</legend>
          {progression !== null ? <ProgressionCard progression={progression} /> : null}
          {badges !== null && badges.length > 0 ? (
            <div className="badge-row" aria-label="Badges">
              {badges.map((badge) => (
                <span key={badge.badgeCode} className="badge-chip" title={badge.badgeCode}>
                  🏅 {badge.badgeCode}
                </span>
              ))}
            </div>
          ) : null}
        </fieldset>
      ) : null}

      <label className="field-label">
        Display name
        <input
          type="text"
          className="input"
          value={displayName}
          onChange={(event) => setDisplayName(event.target.value)}
          maxLength={64}
          placeholder="Your name"
        />
      </label>

      <label className="field-label">
        Bio
        <textarea
          className="input"
          rows={3}
          value={bio}
          onChange={(event) => setBio(event.target.value)}
          maxLength={280}
          placeholder="A line about you"
        />
      </label>

      <label className="field-label">
        Custom status
        <input
          type="text"
          className="input"
          value={customStatus}
          onChange={(event) => setCustomStatus(event.target.value)}
          maxLength={100}
          placeholder="What are you up to?"
        />
        <span className="hint">Shown beside your presence, everywhere your name appears.</span>
      </label>

      <label className="field-label">
        Birth year <span className="muted">(optional)</span>
        <input
          type="number"
          className="input"
          value={birthYear}
          onChange={(event) => setBirthYear(event.target.value)}
          min={BIRTH_YEAR_MIN}
          max={BIRTH_YEAR_MAX}
          placeholder="Not disclosed"
        />
        <span className="hint">Not public; visible only on your own profile panel.</span>
      </label>

      <label className="field-label toggle-field">
        <input
          type="checkbox"
          checked={searchable}
          onChange={(event) => {
            setSearchable(event.target.checked);
            setSearchableTouched(true);
          }}
          aria-label="Appear in username search"
        />
        Appear in username search
        <span className="hint">
          Your current setting is private; the switch joins the save only once you flip it.
        </span>
      </label>

      <fieldset className="panel-section">
        <legend className="panel-heading">Privacy</legend>
        <p className="hint">
          Current settings are private, so each control starts as “Leave as-is”; only a choice you
          make is saved.
        </p>
        {PRIVACY_FIELDS.map((field) => (
          <label className="field-label" key={field.key}>
            {field.label}
            <select
              className="input"
              value={privacy[field.key]}
              onChange={(event) =>
                setPrivacy((prev) => ({ ...prev, [field.key]: event.target.value }))
              }
            >
              <option value={UNCHANGED}>Leave as-is</option>
              {VISIBILITIES.map((option) => (
                <option key={option.value} value={option.value}>
                  {option.label}
                </option>
              ))}
            </select>
            <span className="hint">{field.hint}</span>
          </label>
        ))}
      </fieldset>

      <button
        type="button"
        className="btn btn-primary"
        disabled={!dirty || busy}
        onClick={() => void save()}
      >
        {busy ? <Spinner /> : 'Save changes'}
      </button>
    </div>
  );
}

/**
 * The numeric birth year a draft string names, or `undefined` when it names nothing sendable.
 *
 * An empty field means "do not touch" (the wire never sends the current year back to its owner);
 * a filled field must be a plausible year, because a birth year of "3" or "9999" is a typo, not a
 * fact about a person, and the panel's contract is to send only what it can stand behind.
 */
export function validBirthYear(raw: string): number | undefined {
  const text = raw.trim();
  if (text.length === 0) {
    return undefined;
  }
  const year = Number(text);
  if (!Number.isInteger(year) || year < BIRTH_YEAR_MIN || year > BIRTH_YEAR_MAX) {
    return undefined;
  }
  return year;
}

/**
 * The save patch for a profile edit: exactly the fields that moved.
 *
 * This is where the panel's privacy rule lives, extracted so a test can pin it: the text fields join
 * the patch only when their content differs from the current profile, and a privacy select only when
 * an audience was explicitly chosen ({@link UNCHANGED} means "do not touch"). A user fixing their
 * display name must never re-state a privacy setting they never saw.
 *
 * The extra fields follow the same rule with their own leave-as-is shapes: the birth year joins only
 * when its draft differs from the year the profile carries (the wire echoes it back, so "same year"
 * is knowable and "empty against disclosed" is a change to send, not a guess), and the searchable
 * switch only when it was flipped at all — its untouched state is "do not touch" precisely because
 * the wire's profile carries no current value to pre-select.
 */
export function buildProfilePatch(
  current: UserProfile,
  draft: { displayName: string; bio: string },
  privacy: Record<PrivacyField['key'], string>,
  extra?: { birthYear?: string; searchable?: boolean },
): Partial<ProfileUpdate> {
  const patch: Partial<ProfileUpdate> = {};
  if (draft.displayName.trim() !== current.displayName) {
    patch.displayName = draft.displayName.trim();
  }
  if (draft.bio !== (current.bio ?? '')) {
    patch.bio = draft.bio;
  }
  if (privacy.showLastSeen !== UNCHANGED) {
    patch.showLastSeen = Number(privacy.showLastSeen);
  }
  if (privacy.whoCanMessage !== UNCHANGED) {
    patch.whoCanMessage = Number(privacy.whoCanMessage);
  }
  if (privacy.whoCanAdd !== UNCHANGED) {
    patch.whoCanAdd = Number(privacy.whoCanAdd);
  }
  if (extra !== undefined) {
    const currentYear = current.birthYear === undefined ? '' : String(current.birthYear);
    if ((extra.birthYear ?? '') !== currentYear) {
      const year = validBirthYear(extra.birthYear ?? '');
      // A draft that differs but names no plausible year is a typo, and the panel's
      // contract is to send only what it can stand behind — the field stays untouched
      // rather than the save failing on it.
      if (year !== undefined) {
        patch.birthYear = year;
      }
    }
    if (extra.searchable !== undefined) {
      patch.searchable = extra.searchable;
    }
  }
  return patch;
}
