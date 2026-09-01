#!/usr/bin/env node
/**
 * Migo entity code generator.
 *
 * Reads the SQL migrations in server/migrations and emits one SeaORM entity per table
 * into server/crates/migo-store/src/entity/.
 *
 * The migrations are the source of truth, not a live database and not a hand-written
 * entity module. Three reasons, in order of how much they cost when ignored:
 *
 *   1. The migrations are what actually runs in production, so an entity derived from
 *      them cannot describe a schema that no server has. A `sea-orm-cli generate entity`
 *      run against somebody's laptop describes whatever that laptop has drifted to.
 *   2. No database is needed to regenerate, so the staleness gate runs in the fast CI
 *      job next to `protocol-check` rather than waiting behind a PostgreSQL service.
 *   3. The migrations carry the *reasons* — every non-obvious column in 0001_initial.sql
 *      has a `--` comment explaining it, and those comments become the doc comments on
 *      the generated fields. One place to write down why `authenticated_at` is a
 *      timestamp, and it reaches the Rust reader too.
 *
 * Generated files are committed (ADR-0010) and must never be hand-edited.
 *
 * The parser is deliberately narrow: it understands exactly the DDL this repository
 * writes, and dies with a file:line on anything else. A generator that silently ignores
 * syntax it does not know emits an entity that is wrong in a way nothing catches until a
 * query fails in production.
 *
 * Usage:
 *   node tools/entity-codegen/generate.mjs            # write files
 *   node tools/entity-codegen/generate.mjs --check    # fail if stale (CI gate)
 */
import { readFileSync, writeFileSync, mkdirSync, existsSync, readdirSync, rmSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, resolve, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const ROOT = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const MIGRATIONS = join(ROOT, 'server/migrations');
const OUT_DIR = join(ROOT, 'server/crates/migo-store/src/entity');
const CHECK = process.argv.includes('--check');

const problems = [];
const fail = (where, message) => problems.push(`${where}: ${message}`);

// ---------------------------------------------------------------- SQL type map
//
// Every type this schema uses, and nothing else. `timestamptz` maps to
// `TimeDateTimeWithTimeZone`, which is `time::OffsetDateTime` — the same type the
// store's own conversions already speak, so a row crosses into the domain model
// without a second representation in between.
const TYPES = {
  uuid: { rust: 'Uuid', column: null },
  text: { rust: 'String', column: 'Text' },
  bytea: { rust: 'Vec<u8>', column: 'VarBinary(StringLen::None)' },
  smallint: { rust: 'i16', column: null },
  integer: { rust: 'i32', column: null },
  bigint: { rust: 'i64', column: null },
  boolean: { rust: 'bool', column: null },
  timestamptz: { rust: 'TimeDateTimeWithTimeZone', column: null },
};

// `char(n)` is the one parameterised type in the schema (`account.country`). Kept as a
// separate case because dropping the length would let a three-letter country code
// through a comparison that the database would then pad and refuse to match.
function sqlType(raw, where) {
  const fixed = /^char\s*\(\s*(\d+)\s*\)$/.exec(raw);
  if (fixed) return { rust: 'String', column: `Char(Some(${fixed[1]}))` };
  const known = TYPES[raw];
  if (!known) fail(where, `unknown SQL type "${raw}" — teach the generator or change the column`);
  return known ?? { rust: 'String', column: 'Text' };
}

// ---------------------------------------------------------------- naming
const pascal = (s) => s.replace(/(^|_)([a-z0-9])/g, (_, __, c) => c.toUpperCase());

// ---------------------------------------------------------------- lexing
//
// The migration is read line by line rather than as one blob because comments are
// content here: a `--` block above a column is its documentation, and a trailing `--`
// on the column's own line is usually the enum's numbering. Both are kept.

/** Splits one migration file into statements, carrying the comment lines that precede each. */
function statements(sql, file) {
  const lines = sql.split('\n');
  const out = [];
  let pending = []; // comment lines seen since the last statement
  let current = null;

  lines.forEach((line, index) => {
    const lineNo = index + 1;
    const trimmed = line.trim();

    if (current === null) {
      if (trimmed === '') {
        pending = [];
        return;
      }
      if (trimmed.startsWith('--')) {
        // A ruler (`-- ------`) is a section divider, not documentation.
        if (!/^--\s*-{3,}\s*$/.test(trimmed)) pending.push(trimmed.replace(/^--\s?/, ''));
        return;
      }
      current = { file, line: lineNo, doc: pending, body: [] };
      pending = [];
    }

    current.body.push({ text: line, line: lineNo });
    // The semicolon has to be found in the code, not in the line: this schema contains
    // `-- ... never pass through the API process;` in the middle of a table body, and
    // ending the statement there splits one table into two, the second of which is not
    // a statement at all.
    const code = trimmed.includes('--') ? trimmed.slice(0, trimmed.indexOf('--')).trim() : trimmed;
    if (code.endsWith(';')) {
      out.push(current);
      current = null;
    }
  });

  if (current) fail(`${file}:${current.line}`, 'statement never terminated with a semicolon');
  return out;
}

/**
 * Returns the lines between `create table X (` and the parenthesis that closes it.
 *
 * The close cannot be found by looking at the last parenthesis on the last line, because
 * a partitioned table ends `) partition by range (created_at);` and that would take the
 * body to include the primary key clause and lose it. So the depth is tracked instead.
 */
function tableBody(stmt, where) {
  const stripped = stmt.body.map((entry) => {
    const at = entry.text.indexOf('--');
    return {
      ...entry,
      text: at >= 0 ? entry.text.slice(0, at) : entry.text,
      comment: at >= 0 ? entry.text.slice(at) : '',
    };
  });
  const out = [];
  let depth = 0;
  let open = false;
  for (const entry of stripped) {
    let text = '';
    let closed = false;
    for (const ch of entry.text) {
      if (ch === '(') {
        depth += 1;
        if (!open) {
          open = true;
          continue; // the table's own opening parenthesis is not part of the body
        }
      } else if (ch === ')') {
        depth -= 1;
        if (depth === 0) {
          closed = true;
          break;
        }
      }
      if (open) text += ch;
    }
    if (text.trim() !== '' || entry.comment !== '')
      out.push({ ...entry, text: text + entry.comment });
    if (closed) return out;
  }
  fail(where, 'table body is never closed');
  return out;
}

/**
 * Splits a parenthesised table body on commas that are not inside parentheses.
 *
 * The comment on a line is separated from the code before the commas are looked for, and
 * that ordering is the whole subtlety here. A column written
 *
 *     subject_kind smallint not null,   -- 0 user, 1 message, 2 room
 *
 * ends its item at the comma, so everything after the comma — including the comment — used
 * to be carried into the *next* item's buffer. The generated entity then documented
 * `subject_id` with the numbering of `subject_kind`, and `created_at` with the numbering of
 * `status`: doc comments that were not merely missing but actively wrong, describing one
 * column while sitting on another. So a comment that follows the last comma on its line is
 * appended to the item that comma closed, which is the column the author wrote it beside.
 */
function topLevelItems(body, where) {
  const items = [];
  let depth = 0;
  let buffer = [];
  for (const entry of body) {
    // A comma inside a comment is prose, not a separator. `-- 0 pending, 1 actioned`
    // would otherwise split one column into three items, two of which parse as columns
    // named `1` and `2`.
    const at = entry.text.indexOf('--');
    const code = at >= 0 ? entry.text.slice(0, at) : entry.text;
    const comment = at >= 0 ? entry.text.slice(at) : '';
    let text = '';
    let closed = null;
    for (const ch of code) {
      if (ch === '(') depth += 1;
      if (ch === ')') depth -= 1;
      if (ch === ',' && depth === 0) {
        buffer.push({ ...entry, text });
        items.push(buffer);
        closed = buffer;
        buffer = [];
        text = '';
        continue;
      }
      text += ch;
    }
    if (comment !== '' && text.trim() === '' && closed !== null && closed.length > 0) {
      // Appended to the closed item's last line rather than pushed as a line of its own,
      // because `splitItem` reads a comment that stands alone *after* the code as prose to
      // discard, and only a comment sharing a line with code becomes the column's doc.
      closed[closed.length - 1].text += ` ${comment}`;
    } else {
      text += comment;
    }
    buffer.push({ ...entry, text });
  }
  if (depth !== 0) fail(where, 'unbalanced parentheses in table body');
  if (buffer.some((e) => e.text.trim() !== '')) items.push(buffer);
  return items;
}

/**
 * Pulls the documentation and the code out of one comma-separated table-body item.
 *
 * Preceding `--` lines are the prose; a trailing `--` on the code line is appended to it,
 * because in this schema that trailing form is where enum numbering is written down and
 * losing it would leave `pub status: i16` with nothing to say what 2 means.
 */
function splitItem(entry) {
  const doc = [];
  const code = [];
  let trailing = null;
  for (const line of entry) {
    const trimmed = line.text.trim();
    if (trimmed === '') continue;
    if (trimmed.startsWith('--')) {
      if (code.length === 0) doc.push(trimmed.replace(/^--\s?/, ''));
      continue;
    }
    const at = trimmed.indexOf('--');
    if (at >= 0) {
      code.push(trimmed.slice(0, at).trim());
      trailing = trimmed.slice(at + 2).trim();
    } else {
      code.push(trimmed);
    }
  }
  if (trailing) doc.push(trailing);
  return { doc, code: code.join(' ').replace(/\s+/g, ' ').trim() };
}

/**
 * Parses one comma-separated table-body item (or an `add column` definition) into
 * the table: a `primary key` clause, a `foreign key` clause, a check-style
 * constraint to skip, or a column. Returns the column it added, if any.
 */
function parseItem(table, raw, at) {
  const { doc, code } = splitItem(raw);
  if (code === '') return null;

  const pk = /^primary key \(([^)]+)\)$/i.exec(code);
  if (pk) {
    table.primaryKey = pk[1].split(',').map((c) => c.trim());
    return null;
  }
  const fk =
    /^foreign key \(([^)]+)\) references (\w+) \(([^)]+)\)(?: on delete (\w+(?: \w+)?))?$/i.exec(
      code,
    );
  if (fk) {
    table.foreignKeys.push({
      columns: fk[1].split(',').map((c) => c.trim()),
      table: fk[2],
      references: fk[3].split(',').map((c) => c.trim()),
      onDelete: fk[4] ?? null,
    });
    return null;
  }
  // Row-level invariants are enforced by the database and cannot be expressed on a
  // struct, so they are recognised in order to be skipped rather than misread as a
  // column called `check`.
  if (/^(check|unique|constraint) /i.test(code)) return null;

  const column = /^(\w+)\s+([a-z]+(?:\s*\(\s*\d+\s*\))?)\s*(.*)$/i.exec(code);
  if (!column) {
    fail(at, `unrecognised table-body item: ${code.slice(0, 72)}`);
    return null;
  }
  const [, colName, typeText, rest] = column;
  const modifiers = rest.trim();

  const inlineFk = /references (\w+) \((\w+)\)(?: on delete (\w+(?: \w+)?))?/i.exec(modifiers);
  if (inlineFk) {
    table.foreignKeys.push({
      columns: [colName],
      table: inlineFk[1],
      references: [inlineFk[2]],
      onDelete: inlineFk[3] ?? null,
    });
  }
  if (/^primary key\b/i.test(modifiers)) table.primaryKey.push(colName);

  // Everything a column may say after its type. Anything else is a modifier the
  // generator has never seen, and guessing at it is how a nullable column becomes
  // a non-null field that panics on the first NULL.
  const leftovers = modifiers
    .replace(/primary key/i, '')
    .replace(/not null/i, '')
    .replace(/references \w+ \(\w+\)(?: on delete \w+(?: \w+)?)?/i, '')
    .replace(/default [^,]*/i, '')
    .replace(/\bunique\b/i, '')
    .trim();
  if (leftovers !== '') fail(at, `unrecognised column modifier: "${leftovers}"`);

  const parsed = {
    name: colName,
    doc,
    type: sqlType(typeText.replace(/\s+/g, '').toLowerCase(), at),
    // A primary key is not null whether or not anyone wrote it down.
    nullable: !/not null/i.test(modifiers) && !/^primary key\b/i.test(modifiers),
  };
  table.columns.push(parsed);
  return parsed;
}

// ---------------------------------------------------------------- parsing
const tables = [];

for (const file of readdirSync(MIGRATIONS)
  .filter((f) => f.endsWith('.sql'))
  .sort()) {
  const sql = readFileSync(join(MIGRATIONS, file), 'utf8');

  for (const stmt of statements(sql, file)) {
    const where = `${stmt.file}:${stmt.line}`;
    const head = stmt.body
      .map((e) => e.text)
      .join(' ')
      .replace(/\s+/g, ' ')
      .trim();

    // Indexes shape performance, not the row, so they carry no information an entity
    // could hold. Named explicitly rather than skipped by default, so that a statement
    // form nobody anticipated still reaches the error below.
    if (/^create (unique )?index /i.test(head)) continue;
    // A partition has the parent's columns by definition; the entity addresses the parent.
    if (/^create table \w+ partition of /i.test(head)) continue;

    // `alter table X add column …` grows a table an earlier migration created. The
    // definition is parsed by exactly the rules a create-table column is, then merged
    // into that table's entry: the entities must describe each table as it exists
    // after *every* migration has run, not as the migration that created it left it.
    // A column the table already has is a mistake in the migration, and the generator
    // says so rather than emitting a struct with two fields of the same name.
    const alter = /^alter table (\w+) add column (.*);\s*$/i.exec(head);
    if (alter) {
      const [, name, definition] = alter;
      const target = tables.find((t) => t.name === name);
      if (!target) {
        fail(where, `alter table adds a column to "${name}", which no earlier migration creates`);
        continue;
      }
      const entry = stmt.doc
        .map((text) => ({ text: `-- ${text}`, line: stmt.line }))
        .concat([{ text: definition, line: stmt.line }]);
      const newName = /^(\w+)\s/.exec(definition)?.[1];
      if (newName && target.columns.some((c) => c.name === newName)) {
        fail(where, `alter table adds "${newName}" to "${name}", which already has it`);
        continue;
      }
      parseItem(target, entry, where);
      continue;
    }

    const open = /^create table (\w+) \(/i.exec(head);
    if (!open) {
      fail(where, `unrecognised statement: ${head.slice(0, 72)}…`);
      continue;
    }

    const name = open[1];
    const inner = tableBody(stmt, where);

    const table = {
      name,
      where,
      doc: stmt.doc,
      partitioned: / partition by /i.test(head),
      columns: [],
      primaryKey: [],
      foreignKeys: [],
    };

    for (const raw of topLevelItems(inner, where)) {
      parseItem(table, raw, `${stmt.file}:${raw[0].line}`);
    }

    if (table.primaryKey.length === 0) fail(where, `table ${name} has no primary key`);
    for (const key of table.primaryKey) {
      const column = table.columns.find((c) => c.name === key);
      if (!column) fail(where, `primary key names "${key}", which is not a column`);
      else column.nullable = false;
    }
    tables.push(table);
  }
}

// A foreign key to a table that does not exist would emit an entity referring to a
// module that was never written, which is a compile error a long way from its cause.
const byName = new Map(tables.map((t) => [t.name, t]));
for (const table of tables) {
  for (const fk of table.foreignKeys) {
    if (!byName.has(fk.table))
      fail(table.where, `foreign key references unknown table "${fk.table}"`);
  }
}

if (problems.length > 0) {
  console.error('The generator does not understand the schema:\n');
  for (const p of problems) console.error(`  ${p}`);
  console.error('\nNothing was written.');
  process.exit(1);
}

// ---------------------------------------------------------------- relations
//
// Variant names come from the referenced table, except where one table points at
// another twice — `relationship` has both `account_id` and `other_id` — in which case
// the column names the variant, because two variants called `Account` do not compile.
// A doubled reference also makes `Related` ambiguous, so it is left unimplemented
// there: `find_related` cannot know which edge was meant, and picking one silently
// would answer a different question than the caller asked.
for (const table of tables) {
  const counts = new Map();
  for (const fk of table.foreignKeys) counts.set(fk.table, (counts.get(fk.table) ?? 0) + 1);
  for (const fk of table.foreignKeys) {
    const ambiguous = counts.get(fk.table) > 1;
    fk.variant = ambiguous
      ? pascal(fk.columns.map((c) => c.replace(/_id$/, '')).join('_'))
      : pascal(fk.table);
    fk.related = !ambiguous && fk.table !== table.name;
  }
}

// ---------------------------------------------------------------- emit
const banner =
  '// @generated by tools/entity-codegen/generate.mjs — DO NOT EDIT.\n' +
  '// Source of truth: server/migrations/*.sql.\n' +
  '// Regenerate with: make entities\n';

const docLines = (lines, indent) =>
  lines.length === 0
    ? ''
    : lines.map((l) => (l === '' ? `${indent}///` : `${indent}/// ${l}`)).join('\n') + '\n';

const ON_DELETE = {
  cascade: 'Cascade',
  restrict: 'Restrict',
  'set null': 'SetNull',
  'no action': 'NoAction',
};

function genTable(table) {
  const module = table.name;
  let out = banner;
  out += `//! The \`${module}\` table.\n`;
  if (table.doc.length > 0) out += '//!\n' + table.doc.map((l) => `//! ${l}`).join('\n') + '\n';
  if (table.partitioned) {
    out += '//!\n';
    out += '//! Partitioned by range. Queries address this parent; PostgreSQL routes the row.\n';
  }
  out += '\nuse sea_orm::entity::prelude::*;\n\n';

  const composite = table.primaryKey.length > 1;
  out += docLines(
    [`One row of \`${module}\`.`].concat(
      composite ? ['', `The primary key is (${table.primaryKey.join(', ')}).`] : [],
    ),
    '',
  );
  out += '#[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]\n';
  out += `#[sea_orm(table_name = "${module}")]\n`;
  out += 'pub struct Model {\n';
  for (const column of table.columns) {
    const doc = column.doc.length > 0 ? column.doc : [`The \`${column.name}\` column.`];
    out += docLines(doc, '    ');
    const attrs = [];
    if (table.primaryKey.includes(column.name)) {
      // Every key in this schema is application-generated (UUIDv7, or a natural key).
      // Without `auto_increment = false` SeaORM would omit the column from an insert and
      // wait for the database to invent one.
      attrs.push('primary_key', 'auto_increment = false');
    }
    if (column.type.column) attrs.push(`column_type = "${column.type.column}"`);
    if (attrs.length > 0) out += `    #[sea_orm(${attrs.join(', ')})]\n`;
    const rust = column.nullable ? `Option<${column.type.rust}>` : column.type.rust;
    out += `    pub ${column.name}: ${rust},\n`;
  }
  out += '}\n\n';

  out += docLines([`Foreign keys leaving \`${module}\`.`], '');
  out += '#[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]\n';
  if (table.foreignKeys.length === 0) {
    out += 'pub enum Relation {}\n';
  } else {
    out += 'pub enum Relation {\n';
    for (const fk of table.foreignKeys) {
      out += docLines(
        [`\`${fk.columns.join(', ')}\` → \`${fk.table} (${fk.references.join(', ')})\`.`],
        '    ',
      );
      const from = fk.columns.map((c) => `Column::${pascal(c)}`);
      const to = fk.references.map((c) => `super::${fk.table}::Column::${pascal(c)}`);
      const wrap = (parts) => (parts.length > 1 ? `(${parts.join(', ')})` : parts[0]);
      out += '    #[sea_orm(\n';
      out += `        belongs_to = "super::${fk.table}::Entity",\n`;
      out += `        from = "${wrap(from)}",\n`;
      out += `        to = "${wrap(to)}"`;
      if (fk.onDelete) {
        const action = ON_DELETE[fk.onDelete.toLowerCase()];
        if (!action) throw new Error(`${table.where}: unknown on delete action "${fk.onDelete}"`);
        out += `,\n        on_delete = "${action}"`;
      }
      out += '\n    )]\n';
      out += `    ${fk.variant},\n`;
    }
    out += '}\n';
  }

  for (const fk of table.foreignKeys.filter((f) => f.related)) {
    out += `\nimpl Related<super::${fk.table}::Entity> for Entity {\n`;
    out += '    fn to() -> RelationDef {\n';
    out += `        Relation::${fk.variant}.def()\n`;
    out += '    }\n}\n';
  }

  out += '\nimpl ActiveModelBehavior for ActiveModel {}\n';
  return out;
}

function genMod() {
  let out = banner;
  out += '//! SeaORM entities, one module per table in `server/migrations`.\n';
  out += '//!\n';
  out += '//! These are an implementation detail of the PostgreSQL backend and are not part of\n';
  out +=
    "//! this crate's public API. The traits in [`crate::traits`] speak the domain models in\n";
  out +=
    '//! [`crate::model`], so nothing above the store has to know that SeaORM exists — which\n';
  out += '//! is the property that lets the ORM be replaced without touching a caller.\n';
  out += '//!\n';
  out += `//! ${tables.length} tables. Regenerate with \`make entities\` after changing a migration.\n\n`;
  // The entity set mirrors the migrations, not the code: `make entity-check` fails if a
  // table has no module, so a table nothing reads yet still has one. Denying dead code
  // here would mean the choice is either an unused warning or a hand-written exception
  // list that goes stale — and the whole point of generating these is that no such list
  // exists. The traits are the API; an entity nobody has needed yet is not a defect.
  out += '#![allow(dead_code)]\n\n';
  for (const table of tables) {
    out += `pub mod ${table.name};\n`;
  }
  return out;
}

function rustfmt(source, label) {
  const result = spawnSync('rustfmt', ['--edition', '2021', '--emit', 'stdout'], {
    input: source,
    encoding: 'utf8',
    maxBuffer: 64 * 1024 * 1024,
  });
  if (result.error?.code === 'ENOENT') {
    console.error(
      'rustfmt is not on PATH. Generated Rust must be formatted or the fmt and\n' +
        'entity-check gates disagree — run `rustup component add rustfmt`.',
    );
    process.exit(1);
  }
  if (result.status !== 0) {
    console.error(`rustfmt rejected the generated ${label}:\n${result.stderr}`);
    process.exit(1);
  }
  return result.stdout;
}

const targets = tables
  .map((t) => ({
    path: join(OUT_DIR, `${t.name}.rs`),
    content: rustfmt(genTable(t), `${t.name}.rs`),
  }))
  .concat([{ path: join(OUT_DIR, 'mod.rs'), content: rustfmt(genMod(), 'mod.rs') }]);

// A table that was renamed leaves its old entity behind, and a stale entity compiles
// perfectly well against a column that no longer exists. So the directory is treated as
// the generator's output in full, not as a place it adds files to.
const expected = new Set(targets.map((t) => t.path));
const orphans = existsSync(OUT_DIR)
  ? readdirSync(OUT_DIR)
      .filter((f) => f.endsWith('.rs'))
      .map((f) => join(OUT_DIR, f))
      .filter((p) => !expected.has(p))
  : [];

let stale = false;
for (const target of targets) {
  const existing = existsSync(target.path) ? readFileSync(target.path, 'utf8') : null;
  if (existing === target.content) continue;
  if (CHECK) {
    stale = true;
    console.error(`STALE       ${target.path.replace(ROOT + '/', '')}`);
    continue;
  }
  mkdirSync(dirname(target.path), { recursive: true });
  writeFileSync(target.path, target.content);
}
for (const orphan of orphans) {
  if (CHECK) {
    stale = true;
    console.error(`ORPHAN      ${orphan.replace(ROOT + '/', '')}`);
    continue;
  }
  rmSync(orphan);
  console.log(`removed     ${orphan.replace(ROOT + '/', '')}`);
}

if (CHECK && stale) {
  console.error('\ngenerated entities are stale — run `make entities` and commit the result');
  process.exit(1);
}

const columns = tables.reduce((n, t) => n + t.columns.length, 0);
const keys = tables.reduce((n, t) => n + t.foreignKeys.length, 0);
console.log(
  `${CHECK ? 'up to date: ' : 'written: '}${tables.length} entities, ${columns} columns, ${keys} foreign keys`,
);
