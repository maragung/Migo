import type { ReactNode } from 'react';

import { PresenceState } from '@migo/sdk';

import { hueFromId, initials } from '@/lib/format.js';

import { PresenceDot } from './presence-dot.js';

interface AvatarProps {
  /** The display name, for initials and the accessible label. */
  name: string;
  /** A stable id, so the fallback colour is consistent across sessions. */
  id: string;
  /** Pixel diameter. Defaults to 40. */
  size?: number;
  /** An optional avatar image URL. */
  avatarUrl?: string;
  /** An optional presence badge in the corner. */
  presence?: PresenceState;
}

/** A circular avatar: the image when present, otherwise coloured initials. */
export function Avatar({ name, id, size = 40, avatarUrl, presence }: AvatarProps): ReactNode {
  const hue = hueFromId(id);
  const style = {
    width: size,
    height: size,
    fontSize: Math.round(size * 0.4),
    background: avatarUrl
      ? undefined
      : `linear-gradient(135deg, hsl(${hue} 60% 55%), hsl(${(hue + 40) % 360} 60% 45%))`,
  };
  return (
    <span className="avatar" style={style} aria-label={name}>
      {avatarUrl ? <img src={avatarUrl} alt="" /> : initials(name)}
      {presence !== undefined ? <PresenceDot state={presence} /> : null}
    </span>
  );
}
