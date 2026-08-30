'use client';

import { useCallback, useEffect, useRef, useState } from 'react';
import type { ChangeEvent, ReactNode } from 'react';

import type { ProfileUpdate, UserProfile } from '@migo/sdk';

import { friendlyError } from '@/lib/migo/errors.js';
import { uploadAvatarMedia } from '@/lib/migo/media.js';
import { useMigo } from '@/lib/migo/use-migo.js';

import { Avatar } from './avatar.js';
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
 */
export function ProfilePanel(): ReactNode {
  const { client, accountId } = useMigo();

  const [profile, setProfile] = useState<UserProfile | null>(null);
  const [displayName, setDisplayName] = useState('');
  const [bio, setBio] = useState('');
  const [privacy, setPrivacy] = useState<Record<PrivacyField['key'], string>>({
    showLastSeen: UNCHANGED,
    whoCanMessage: UNCHANGED,
    whoCanAdd: UNCHANGED,
  });
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
    client.profile
      .fetchOne(accountId)
      .then((fetched) => {
        if (!cancelled && fetched !== null) {
          setProfile(fetched);
        }
      })
      .catch(() => {
        if (!cancelled) {
          setError('Could not load your profile.');
        }
      });
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
    setPrimed(true);
  }, [profile, primed]);

  const save = useCallback(async (): Promise<void> => {
    if (!client || !profile || busy) {
      return;
    }
    const patch = buildProfilePatch(profile, { displayName, bio }, privacy);
    if (Object.keys(patch).length === 0) {
      return;
    }

    setBusy(true);
    setError(null);
    setSaved(false);
    try {
      const updated = await client.profile.updateProfile(patch);
      // The reply is the authoritative profile; adopt it wholesale so the view never disagrees
      // with the server about what was saved.
      setProfile(updated);
      setDisplayName(updated.displayName);
      setBio(updated.bio ?? '');
      setPrivacy({
        showLastSeen: UNCHANGED,
        whoCanMessage: UNCHANGED,
        whoCanAdd: UNCHANGED,
      });
      setSaved(true);
    } catch (cause) {
      setError(friendlyError(cause));
    } finally {
      setBusy(false);
    }
  }, [client, profile, busy, displayName, bio, privacy]);

  /**
   * Uploads a picked image as the new avatar, then patches the profile to point at it.
   *
   * The two steps are one action to the user, so the busy state covers both and a failure anywhere
   * surfaces beside the picker that started it. The reply is the authoritative profile, adopted
   * wholesale exactly like {@link save} does.
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
        setProfile(updated);
        setDisplayName(updated.displayName);
        setBio(updated.bio ?? '');
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
          </div>
        </div>
      ) : null}

      {error ? <p className="form-error">{error}</p> : null}
      {saved ? <p className="hint">Profile saved.</p> : null}

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
 * The save patch for a profile edit: exactly the fields that moved.
 *
 * This is where the panel's privacy rule lives, extracted so a test can pin it: the text fields join
 * the patch only when their content differs from the current profile, and a privacy select only when
 * an audience was explicitly chosen ({@link UNCHANGED} means "do not touch"). A user fixing their
 * display name must never re-state a privacy setting they never saw.
 */
export function buildProfilePatch(
  current: UserProfile,
  draft: { displayName: string; bio: string },
  privacy: Record<PrivacyField['key'], string>,
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
  return patch;
}
