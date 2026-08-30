'use client';

/**
 * The design system page: the source of truth, rendered.
 *
 * Every token scale and shared component the product is built from, on one page — colours (both
 * themes), typography, spacing, the controls, the list rows, the states — so a future Migo
 * surface can be built against what exists instead of inventing a parallel vocabulary. The page
 * is itself built only from the tokens and components it documents; anything it cannot draw
 * from the system does not belong on it.
 */

import type { ReactNode } from 'react';

import { Avatar } from '@/components/avatar.js';
import { CoinMark } from '@/components/icons.js';
import { Icon } from '@/components/icons.js';
import type { IconName } from '@/components/icons.js';
import { EmptyState } from '@/components/states.js';
import { Skeleton } from '@/components/states.js';
import { ThemeToggle } from '@/components/theme-toggle.js';

/** The token names the colour section documents, in the order the tokens file groups them. */
const COLOUR_TOKENS: ReadonlyArray<{ name: string; note: string }> = [
  { name: '--bg', note: 'app background' },
  { name: '--panel', note: 'surfaces' },
  { name: '--panel-alt', note: 'sunken surfaces' },
  { name: '--input', note: 'fields' },
  { name: '--border', note: 'hairlines' },
  { name: '--border-strong', note: 'strong lines' },
  { name: '--text', note: 'primary ink' },
  { name: '--text-dim', note: 'secondary ink' },
  { name: '--text-faint', note: 'tertiary ink' },
  { name: '--accent', note: 'the Migo accent' },
  { name: '--accent-strong', note: 'accent, pressed' },
  { name: '--positive', note: 'presence, success' },
  { name: '--warning', note: 'caution' },
  { name: '--danger', note: 'destructive' },
  { name: '--offline', note: 'absence' },
  { name: '--gold', note: 'badges, honours' },
];

/** The type scale, each step drawn at its own size. */
const TYPE_SCALE: ReadonlyArray<{ name: string; size: string; sample: string }> = [
  { name: 'micro', size: 'var(--fs-micro)', sample: 'COMPACT BY DESIGN' },
  { name: 'meta', size: 'var(--fs-meta)', sample: 'Metadata and timestamps' },
  { name: 'body-sm', size: 'var(--fs-body-sm)', sample: 'Secondary body text' },
  { name: 'body', size: 'var(--fs-body)', sample: 'Message text and controls' },
  { name: 'title-sm', size: 'var(--fs-title-sm)', sample: 'Section subtitles' },
  { name: 'title', size: 'var(--fs-title)', sample: 'Panel titles' },
  { name: 'display', size: 'var(--fs-display)', sample: 'The greeting' },
];

/** The icon family, every glyph at its native 20px. */
const ICONS: ReadonlyArray<IconName> = [
  'home',
  'chats',
  'rooms',
  'space',
  'friends',
  'bell',
  'search',
  'wallet',
  'user',
  'settings',
  'plus',
  'send',
  'smile',
  'attach',
  'mic',
  'gift',
  'game',
  'star',
  'verified',
  'back',
  'chevron-right',
  'menu',
  'close',
  'sun',
  'moon',
  'signout',
  'coins',
  'refresh',
  'sparkle',
  'shield',
  'pin',
];

export default function DesignPage(): ReactNode {
  return (
    <div className="panel panel-wide">
      <header className="panel-head">
        <h1 className="panel-title">Design system</h1>
        <ThemeToggle />
      </header>
      <p className="muted">
        One Migo identity: the tokens, the type, the icons, and the shared components every screen
        is built from. The canonical source is <code>shared/design/tokens.json</code>.
      </p>

      <section className="panel-section" aria-label="Colour tokens">
        <h2 className="panel-heading">Colour</h2>
        <ul className="ds-swatches">
          {COLOUR_TOKENS.map((token) => (
            <li key={token.name} className="ds-swatch">
              <span className="ds-swatch-chip" style={{ background: `var(${token.name})` }} />
              <span className="ds-swatch-name">{token.name}</span>
              <span className="ds-swatch-note">{token.note}</span>
            </li>
          ))}
        </ul>
      </section>

      <section className="panel-section" aria-label="Typography">
        <h2 className="panel-heading">Typography</h2>
        <ul className="ds-type">
          {TYPE_SCALE.map((step) => (
            <li key={step.name} className="ds-type-step">
              <span className="ds-type-name">{step.name}</span>
              <span style={{ fontSize: step.size }}>{step.sample}</span>
            </li>
          ))}
        </ul>
      </section>

      <section className="panel-section" aria-label="Spacing and radius">
        <h2 className="panel-heading">Spacing &amp; radius</h2>
        <div className="ds-spacing">
          {[1, 2, 3, 4, 6, 8, 12].map((step) => (
            <span key={step} className="ds-spacing-step">
              <span className="ds-spacing-bar" style={{ width: `calc(${step} * var(--sp-1))` }} />
              {step}
            </span>
          ))}
        </div>
        <div className="ds-spacing">
          {[
            { name: 'sm', radius: 'var(--radius-sm)' },
            { name: 'md', radius: 'var(--radius)' },
            { name: 'lg', radius: 'var(--radius-lg)' },
            { name: 'pill', radius: '999px' },
          ].map((step) => (
            <span key={step.name} className="ds-spacing-step">
              <span className="ds-radius-box" style={{ borderRadius: step.radius }} />
              {step.name}
            </span>
          ))}
        </div>
      </section>

      <section className="panel-section" aria-label="Icons">
        <h2 className="panel-heading">Icons — one family, one stroke</h2>
        <ul className="ds-icons">
          {ICONS.map((name) => (
            <li key={name} className="ds-icon" title={name}>
              <Icon name={name} size={20} />
              <span className="ds-icon-name">{name}</span>
            </li>
          ))}
          <li className="ds-icon" title="coin">
            <CoinMark size={20} />
            <span className="ds-icon-name">$MIG</span>
          </li>
        </ul>
      </section>

      <section className="panel-section" aria-label="Buttons">
        <h2 className="panel-heading">Buttons</h2>
        <div className="ds-row">
          <button type="button" className="btn btn-primary">
            Primary
          </button>
          <button type="button" className="btn">
            Default
          </button>
          <button type="button" className="btn btn-ghost">
            Ghost
          </button>
          <button type="button" className="btn btn-danger">
            Danger
          </button>
          <button type="button" className="btn btn-primary btn-sm">
            Small
          </button>
          <button type="button" className="icon-btn" aria-label="Icon">
            <Icon name="plus" size={20} />
          </button>
        </div>
      </section>

      <section className="panel-section" aria-label="Chips">
        <h2 className="panel-heading">Chips</h2>
        <div className="ds-row">
          <span className="mig-chip">
            <CoinMark size={14} />
            $MIG
          </span>
          <button type="button" className="chip chip-active">
            Active
          </button>
          <button type="button" className="chip">
            Filter
          </button>
          <button type="button" className="token-ref">
            $MIG
          </button>
        </div>
      </section>

      <section className="panel-section" aria-label="List rows">
        <h2 className="panel-heading">List rows</h2>
        <ul className="digest-list">
          <li>
            <div className="digest-row digest-row-static">
              <Avatar name="Ada" id="ds-1" size={32} />
              <span className="digest-main">
                <span className="person-name">Ada</span>
                <span className="person-sub">@ada · last seen 2m ago</span>
              </span>
              <span className="unread-dot" aria-label="Unread" />
            </div>
          </li>
          <li>
            <div className="digest-row digest-row-static">
              <span className="digest-glyph" aria-hidden="true">
                <Icon name="gift" size={20} />
              </span>
              <span className="digest-main">
                <span className="person-name">Gift received</span>
                <span className="person-sub">Economy · just now</span>
              </span>
            </div>
          </li>
        </ul>
      </section>

      <section className="panel-section" aria-label="States">
        <h2 className="panel-heading">States</h2>
        <Skeleton rows={2} />
        <EmptyState
          icon="space"
          title="Nothing here yet."
          hint="Empty states name what would fill them."
        />
      </section>
    </div>
  );
}
