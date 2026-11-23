import type { CommandRunResult, EngineInterface, Register, TurnCompleteReason } from 'claude-code'
import type { AiRecall } from '../types'

const NOT_ATTACHED = 3
const LONG_MS = 600_000
const FLUSH_MS = 1_100
const USAGE = 'Usage: /ai new <name> | /ai attach <name> | /ai detach'
/** What `/fold` sends the session as the user's words, before its direction, once the command has left the model the instructions for a handoff. */
const REQUEST = 'Fold this session.'
/** Why a handoff turn that ended for `reason` wrote no handoff. */
const UNWRITTEN: Record<TurnCompleteReason, string> = {
  answer: 'the handoff turn ended with no answer',
  aborted: 'the handoff was stopped',
  refusal: 'the model refused to write the handoff',
  error: 'the handoff turn failed',
}
const WITHHELD =
  'Not folded: this session is attached to no agent, so a fold compacts it, and the ai plugin never saw this session start, so it would not see the compaction either: Claude Code would write its own summary. Where an organization manages Claude Code, only plugins its managed settings install see these events.'
const STAYS_ON = 'Automatic compaction stays on'
const RECALL_USAGE = 'Usage: /recall <message>'
const RELAY =
  "/recall started a read-only fork on the user's message. When its brief arrives, show it to the user whole and wait for their review. Then write the whole brief again with their corrections, and note nothing from it they have not approved."
const REFUSED =
  'A recall only reads: Read, Grep, Glob, and one read-only ai, git or but command per Bash call, with no pipes, redirections or substitutions (quote patterns in single quotes).'

const transcript = { plugin: 'ai', key: 'transcript' } as const
const prepared = { plugin: 'ai', key: 'compaction' } as const
const recalls = { plugin: 'ai', key: 'recall' } as const

/** The tools a recall fork may call besides Bash. */
const READERS = new Set(['Read', 'Grep', 'Glob'])
/** The commands a recall fork may run in Bash, by their leading words. */
const READS = [
  ...['view', 'zoom', 'grep', 'show'].map(verb => ['ai', 'memory', verb]),
  ...['view', 'zoom', 'grep', 'list'].map(verb => ['ai', 'agent', verb]),
  ...['view', 'zoom', 'grep', 'show'].map(verb => ['ai', 'notes', verb]),
  ['ai', 'view'],
  ['ai', 'page'],
  ...['log', 'show', 'diff', 'status'].map(verb => ['git', verb]),
  ...['status', 'diff', 'show'].map(verb => ['but', verb]),
]
/** Characters that make a shell do more than split words: operators, redirections, expansions, globs, comments. */
const SPECIAL = /[\n\r;&|<>()$`\\*?[\]{}~#!]/

type Ran = { exitCode: number; stdout: string; stderr: string }

async function ai($: EngineInterface, args: string[], timeoutMs?: number): Promise<Ran> {
  try {
    return await $.process.run(['ai', ...args], { timeoutMs })
  } catch (err) {
    return { exitCode: -1, stdout: '', stderr: String(err) }
  }
}

/** Runs `ai agent args`, logging what a run that went through warned of. */
async function agent($: EngineInterface, args: string[], timeoutMs?: number): Promise<Ran> {
  const ran = await ai($, ['agent', ...args], timeoutMs)
  const said = ran.stderr.trim()
  if (ran.exitCode === 0 && said !== '') $.ui.log(said)
  return ran
}

async function compactSession($: EngineInterface, args: string[], cancelled?: AbortSignal): Promise<Ran> {
  const out = { stdout: '', stderr: '' }
  const child = $.process.spawn({ argv: ['ai', 'agent', 'compact-session', ...args] })
  const pieces: AsyncIterator<unknown> = child
  const stop = () => void pieces.return?.().catch(() => undefined)
  cancelled?.addEventListener('abort', stop, { once: true })
  try {
    for await (const { stream, text } of child) out[stream] += text
    // A stream closed mid-read resolves `result` with undefined, not the rejection HookStream documents.
    if (cancelled?.aborted) return { exitCode: -1, ...out }
    return { exitCode: (await child.result).code ?? -1, ...out }
  } catch (err) {
    return { exitCode: -1, stdout: out.stdout, stderr: String(err) }
  } finally {
    cancelled?.removeEventListener('abort', stop)
  }
}

function sessionArgs(id: string, path: string | undefined): string[] {
  return ['--harness', 'claude', '--session', id, ...(path === undefined ? [] : ['--transcript', path])]
}

/** The transcript of session `id`, known while `id` is the session whose start the plugin last saw. */
async function transcriptOf($: EngineInterface, id: string): Promise<string | undefined> {
  const seen = (await $.state.get(transcript)).value
  return seen?.session === id ? seen.path : undefined
}

async function session($: EngineInterface): Promise<string[]> {
  const id = await $.session.id()
  return sessionArgs(id, await transcriptOf($, id))
}

async function view($: EngineInterface, args: string[], fallback: string): Promise<string | undefined> {
  const ran = await compactSession($, args)
  if (ran.exitCode === 0 && ran.stdout !== '') return ran.stdout
  if (ran.exitCode !== 0 && ran.exitCode !== NOT_ATTACHED) {
    $.ui.log(`ai agent compact-session failed; ${fallback}. ${ran.stderr.trim()}`)
  }
  return undefined
}

/** Syncs or ends the session, logging why it failed. */
async function sync($: EngineInterface, command: 'sync-session' | 'end-session', args: string[]): Promise<void> {
  const ran = await agent($, [command, ...args])
  if (ran.exitCode !== 0 && ran.exitCode !== NOT_ATTACHED) $.ui.log(`ai agent ${command} failed: ${ran.stderr.trim()}`)
}

function outcome(ran: Ran): string {
  if (ran.exitCode === 0) return ran.stdout.trim()
  if (ran.exitCode === NOT_ATTACHED) return 'This session is not attached to an agent. Attach it with /ai attach <name>.'
  return ran.stderr.trim() || `ai exited with code ${ran.exitCode}.`
}

/** Runs `/rename args` once the calling command has returned, since a command run inside it would wait on it. */
function rename($: EngineInterface, args: string): void {
  $.clock.after(0, () =>
    $.command.run({ command: 'rename', args }).catch(err => $.ui.log(`ai: /rename ${args} failed: ${String(err)}`)),
  )
}

/** Attaches the session to agent `name`, then names it as `ai agent session-name` spells the agent's address. */
async function attach($: EngineInterface, name: string): Promise<string> {
  const attached = await agent($, ['attach', name, ...(await session($))], LONG_MS)
  if (attached.exitCode !== 0) return outcome(attached)
  const named = await agent($, ['session-name', name])
  if (named.exitCode !== 0) return `${outcome(attached)} Not renamed: ${outcome(named)}`
  const title = named.stdout.trim()
  rename($, title)
  return `${outcome(attached)} Renaming this session ${title}.`
}

/** Detaches the session, then has Claude Code name it from its conversation, so the agent's address leaves it. */
async function detach($: EngineInterface): Promise<string> {
  const detached = await agent($, ['detach', ...(await session($))])
  if (detached.exitCode !== 0) return outcome(detached)
  rename($, '')
  return `${outcome(detached)} Renaming this session from its conversation.`
}

async function isWithheld($: EngineInterface): Promise<boolean> {
  return (await $.state.get(transcript)).value === undefined
}

/** Whether a fold is under way: from the end of its handoff's turn until the new session is attached. */
let folding = false

/**
 * The latest `/fold`'s request for a handoff: the message sent as the user's words, the
 * direction the new session starts on ('' for none), and the turn answering it once started.
 */
type Handoff = { request: string; direction: string; turnId?: string }
let handoff: Handoff | undefined

/**
 * Asks the session for its handoff, aimed at `direction`: the command leaves the model the
 * instructions, then `REQUEST`, with `Next:` and the direction after it, goes in as the user's
 * words once the command has returned and the session is idle. The fold follows when
 * `turn.complete` hears that turn end. Returns what the command answers.
 */
async function request($: EngineInterface, direction: string): Promise<CommandRunResult> {
  let instructions: string
  try {
    instructions = await $.fs.read(`${$.plugin.root}/prompts/fold.txt`)
  } catch (err) {
    return { text: await handBack($, `Not folded: ${String(err)}`, direction) }
  }
  const asked: Handoff = { request: direction === '' ? REQUEST : `${REQUEST} Next: ${direction}`, direction }
  handoff = asked
  $.clock.after(0, async () => {
    const refused = await send($, asked.request)
    if (refused === undefined) return
    if (handoff === asked) handoff = undefined
    const said = `Not folded: the request for a handoff was refused (${refused}), so this session is as it was.`
    $.ui.log(await handBack($, said, direction))
  })
  return { text: 'Writing a handoff. The fold follows it.', context: [instructions] }
}

/** Sends `text` to the session as the user's words, once it is idle. Returns why it was refused, else undefined. */
async function send($: EngineInterface, text: string): Promise<string | undefined> {
  try {
    return (await $.prompt.submit({ text, asUser: true })).drop
  } catch (err) {
    return String(err)
  }
}

/**
 * `said`, the reason a fold stops short of starting a session on `direction`, with the direction
 * handed back so none of it is lost: put in the prompt box, after a blank line when the box holds
 * what the user typed since, else, where no box takes it, written out whole. `said` alone for no
 * direction.
 */
async function handBack($: EngineInterface, said: string, direction: string): Promise<string> {
  if (direction === '') return said
  const filled = await $.prompt
    .read()
    .then(({ text }) =>
      $.prompt.fill(
        text.trim() === '' ? { text: direction, mode: 'replace' } : { text: `\n\n${direction}`, mode: 'append' },
      ),
    )
    .catch(() => ({ isFilled: false }))
  // `said` can end in a message of ai's own, with no full stop.
  const ended = /[.!?]$/.test(said) ? said : `${said}.`
  return filled.isFilled
    ? `${ended} Your direction is in the prompt box.`
    : `${ended} The prompt box would not take your direction, which was: ${direction}`
}

/** Starts the folded session on `direction`, or, when `unready` says why it should not, hands it back. */
async function begin($: EngineInterface, direction: string, unready?: string): Promise<void> {
  if (direction === '') return
  const why = unready ?? (await send($, direction))
  if (why === undefined) return
  $.ui.log(await handBack($, `Not started on your direction: ${why}.`, direction))
}

/**
 * Carries the thread of session `old` into a new session: `/clear`, then `view` as the new
 * session's first row, before a notification that waited can start a turn there, then, once
 * both transcripts are flushed, the new session attached where `old` is, and started on
 * `direction`. `/clear` keeps the session's name. Logs what happened: what failed, and what the
 * session holds.
 */
async function carry($: EngineInterface, old: string, view: string, direction: string): Promise<void> {
  try {
    await $.command.run({ command: 'clear' })
  } catch (err) {
    $.ui.log(await handBack($, `Not folded: /clear failed, so this session is as it was. ${String(err)}`, direction))
    return
  }
  const id = await $.session.id()
  if (id === old) {
    $.ui.log(await handBack($, 'Not folded: /clear started no new session, so this session is as it was.', direction))
    return
  }
  let unseen: string | undefined
  try {
    const row = await $.session.append({ message: { type: 'user', content: [{ type: 'text', text: view }] } })
    unseen = row.deny
  } catch (err) {
    unseen = String(err)
  }
  await $.clock.sleep(FLUSH_MS)
  const args = ['carry-session', '--from', old, ...sessionArgs(id, await transcriptOf($, id))]
  const carried = await agent($, args, LONG_MS)
  const holds =
    unseen === undefined
      ? "It holds your memory's view and the thread's."
      : `It holds no view, which was refused (${unseen}): have Claude run ai view.`
  if (carried.exitCode === 0) {
    $.ui.log(`Folded into a new session. ${carried.stdout.trim()} ${holds}`)
  } else {
    const why = carried.exitCode === NOT_ATTACHED ? `Session ${old} is attached to none now.` : carried.stderr.trim()
    $.ui.log(
      `Folded into a new session, which is attached to no agent: ai agent carry-session failed. ${why} ${holds} Attach it with /ai attach <name>.`,
    )
  }
  const unready =
    unseen !== undefined ? 'the new session holds no view' : carried.exitCode !== 0 ? 'it is attached to no agent' : undefined
  await begin($, direction, unready)
}

/** A session whose thread's view `ai agent compact-session` built: its id, its `ai` arguments, and whether it is attached. */
type Built = { old: string; args: string[]; isAttached: boolean }

/**
 * Builds the thread's view of the session once its transcript is flushed, writing the lines it
 * needs, however long they take. Returns the session as built, or why it folds nothing.
 */
async function build($: EngineInterface, cancelled: AbortSignal): Promise<Built | { failed: string }> {
  await $.clock.sleep(FLUSH_MS)
  const old = await $.session.id()
  const args = sessionArgs(old, await transcriptOf($, old))
  const ready = await compactSession($, args, cancelled)
  if (cancelled.aborted) return { failed: 'Not folded: cancelled.' }
  if (ready.exitCode !== 0 && ready.exitCode !== NOT_ATTACHED) return { failed: `Not folded: ${outcome(ready)}` }
  if (ready.exitCode === 0 && ready.stdout === '') return { failed: 'Not folded: the thread holds nothing yet.' }
  const isAttached = ready.exitCode === 0
  if (!isAttached && (await isWithheld($))) return { failed: WITHHELD }
  return { old, args, isAttached }
}

/**
 * Folds the session once it has written `written`, its handoff, while the turn that wrote it
 * ends: the thread's view is built again, its newest turn now the handoff and its lines mostly
 * there from the build `/fold` ran first. Attached to an agent, it prints `ai view` (the
 * memory's view, then the thread's) and hands it to `carry`, which goes on in a new session.
 * Attached to none, it prints `ai view` and runs `/compact`, which `session.compact` answers with
 * that text and the handoff after it. Either way the new session then starts on `direction`.
 * Returns why it folds nothing, else undefined.
 */
async function fold(
  $: EngineInterface,
  written: string,
  direction: string,
  cancelled: AbortSignal,
): Promise<string | undefined> {
  folding = true
  let isCarried = false
  try {
    const built = await build($, cancelled)
    if ('failed' in built) return built.failed
    const shown = await ai($, ['view', '--whole', ...built.args])
    if (shown.exitCode !== 0) return `Not folded: ${outcome(shown)}`
    if (cancelled.aborted) return 'Not folded: cancelled.'
    if (built.isAttached) {
      isCarried = true
      $.clock.after(0, () => carry($, built.old, shown.stdout, direction).finally(() => (folding = false)))
    } else {
      await $.state.set(prepared, { view: shown.stdout, handoff: written })
      $.clock.after(0, () =>
        $.command.run({ command: 'compact' }).then(
          () => begin($, direction),
          async (err: unknown) => $.ui.log(await handBack($, `Not folded: /compact failed. ${String(err)}`, direction)),
        ),
      )
    }
    return undefined
  } finally {
    if (!isCarried) folding = false
  }
}

/** Why the prompt box took no text, by the engine's refusal; a hook's refusal carries none. */
const UNFILLED = {
  dialog: 'a dialog holds the prompt box',
  no_composer: 'this session has no prompt box',
} as const

/**
 * `text` with `> ` before each line, `>` alone on an empty one, without the empty lines it
 * starts or ends with, as `ai reply` quotes.
 */
function quoted(text: string): string {
  return text
    .trimEnd()
    .replace(/^[\r\n]+/, '')
    .split(/\r?\n/)
    .map(line => line.trimEnd())
    .map(line => (line === '' ? '>' : `> ${line}`))
    .join('\n')
}

/**
 * What `/reply` quotes: the replies `refs` picks, as typed, which `ai reply` splits and quotes;
 * with none, the text selected with the mouse, else the latest answer. Either the quote, or why
 * there is none. What `ai reply` warns of beside a quote is logged: it read the transcript alone.
 */
async function quotation($: EngineInterface, refs: string): Promise<{ text: string } | { failed: string }> {
  if (refs === '') {
    const selected = (await $.ui.selection())?.text ?? ''
    if (selected.trim() !== '') return { text: quoted(selected) }
  }
  const ran = await ai($, ['reply', ...(await session($)), '--', ...(refs === '' ? [] : [refs])])
  if (ran.exitCode !== 0) return { failed: outcome(ran) }
  const warned = ran.stderr.trim()
  if (warned !== '') $.ui.log(warned)
  return { text: ran.stdout }
}

/**
 * Puts what `/reply` with `refs` quotes into the prompt box at the cursor, so a draft stays,
 * with an empty line after it to answer on. Returns why it put nothing, else undefined.
 */
async function quote($: EngineInterface, refs: string): Promise<string | undefined> {
  const q = await quotation($, refs)
  if ('failed' in q) return `Not quoted: ${q.failed}`
  const filled = await $.prompt.fill({ text: `${q.text.trimEnd()}\n\n`, mode: 'insert' })
  if (filled.isFilled) return undefined
  const why = filled.refusal === undefined ? 'the prompt box did not take it' : UNFILLED[filled.refusal]
  return `Not quoted: ${why}.`
}

/**
 * `command`'s words when it is one simple command, quoted only in single quotes or in double
 * quotes that hold no expansion; undefined for anything else.
 */
function words(command: string): string[] | undefined {
  const out: string[] = []
  let word: string | undefined
  for (let i = 0; i < command.length; i++) {
    const c = command[i]!
    if (c === ' ' || c === '\t') {
      if (word !== undefined) out.push(word)
      word = undefined
    } else if (c === "'" || c === '"') {
      const end = command.indexOf(c, i + 1)
      if (end < 0) return undefined
      const quoted = command.slice(i + 1, end)
      if (c === '"' && /[$`\\!]/.test(quoted)) return undefined
      word = (word ?? '') + quoted
      i = end
    } else if (SPECIAL.test(c)) {
      return undefined
    } else {
      word = (word ?? '') + c
    }
  }
  if (word !== undefined) out.push(word)
  return out
}

/** Whether `command` is one of `READS`, with no `--output` that would write a file. */
function isRead(command: string): boolean {
  const said = words(command)
  if (said === undefined || said.some(word => word.startsWith('--output'))) return false
  return READS.some(read => read.every((word, i) => said[i] === word))
}

async function recalled($: EngineInterface): Promise<AiRecall> {
  return (await $.state.get(recalls)).value ?? { forks: [], prompts: [] }
}

/** `state` with `prompt` no longer pending, and `fork` added when given. */
function known(state: AiRecall, prompt: string, fork?: string): AiRecall {
  const at = state.prompts.indexOf(prompt)
  const prompts = at < 0 ? state.prompts : state.prompts.toSpliced(at, 1)
  const forks = fork === undefined || state.forks.includes(fork) ? state.forks : [...state.forks, fork]
  return { forks, prompts }
}

/**
 * Starts a read-only fork on `direction`. The guard learns the fork's id from whichever comes
 * first: its first turn's start, which carries the prompt, or the spawn's answer, which covers a
 * prompt another plugin rewrote.
 */
async function recall($: EngineInterface, direction: string): Promise<CommandRunResult> {
  let template: string
  try {
    template = await $.fs.read(`${$.plugin.root}/prompts/recall.txt`)
  } catch (err) {
    return { text: `Not recalling: ${String(err)}` }
  }
  const prompt = template.replace('{direction}', () => direction)
  const before = await recalled($)
  await $.state.set(recalls, { ...before, prompts: [...before.prompts, prompt] })
  const spawned = await $.agent.spawn({ subagentType: 'fork', description: 'Recall', prompt })
  if (spawned.deny !== undefined || spawned.agentId !== undefined) {
    await $.state.set(recalls, known(await recalled($), prompt, spawned.agentId))
  }
  if (spawned.deny !== undefined) return { text: `Not recalling: ${spawned.deny}` }
  return { text: 'Recalling. The brief follows when the fork ends.', context: [RELAY] }
}

/**
 * Turns Claude Code's automatic compaction off for this session, unless the settings hold
 * `autoCompactEnabled: true`; says why when the engine keeps it on.
 */
async function noAutoCompact($: EngineInterface): Promise<void> {
  try {
    if ((await $.settings.read({ key: 'autoCompactEnabled' })) === true) return
    const { deny } = await $.env.set('DISABLE_AUTO_COMPACT', '1')
    if (deny !== undefined) $.ui.log(`${STAYS_ON}: ${deny}`)
  } catch (err) {
    $.ui.log(`${STAYS_ON}: ${String(err)}`)
  }
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await noAutoCompact($)
    await $.command.register({
      name: 'fold',
      description:
        "ai: have Claude write a handoff, aimed at your direction when you give one, then go on in a new session that starts from your memory's view and the thread's, the handoff its newest turn, and starts on your direction; in a session attached to no agent, compact it to your memory's view and the handoff",
      argumentHint: '[direction]',
    })
    await $.command.register({
      name: 'ai',
      description:
        'ai: new <name> creates an agent and attaches this session to it; attach <name> attaches it to an existing agent; both name the session <project>.<name>; detach detaches it and names it from its conversation',
      argumentHint: 'new <name> | attach <name> | detach',
    })
    await $.command.register({
      name: 'recall',
      description:
        'ai: a read-only fork briefs you on your message from your memory, the thread and the repository',
      argumentHint: '<message>',
    })
    await $.command.register({
      name: 'reply',
      description:
        'ai: quote the text you selected, else the latest answer, into the prompt box; -N an older answer, N the answer to your message at turn N, #k one reply in it',
      argumentHint: '[-N | N | #k | -N#k | N#k ...]',
      immediate: true,
    })
    return next(e)
  })

  on('classic.SessionStart', async ($, e, next) => {
    await $.state.set(transcript, { session: e.session_id, path: e.transcript_path })
    const started = await next(e)
    if (e.source !== 'startup') return started
    const seed = await view($, sessionArgs(e.session_id, e.transcript_path), "starting without the thread's view")
    if (seed === undefined) return started
    return { ...started, additionalContext: [...(started.additionalContext ?? []), seed] }
  })

  on('session.compact', async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    const ready = (await $.state.get(prepared)).value
    if (!ready?.view) return next(e)
    await $.state.set(prepared, { view: '', handoff: '' })
    return {
      messages: [
        { role: 'user', text: ready.view, toolUses: [] },
        { role: 'assistant', text: ready.handoff, toolUses: [] },
      ],
    }
  })

  on('classic.Stop', async ($, e, next) => {
    const args = sessionArgs(e.session_id, e.transcript_path)
    $.clock.after(FLUSH_MS, () => sync($, 'sync-session', args))
    return next(e)
  })

  on('session.end', async ($, e, next) => {
    const ended = await next(e)
    await sync($, 'end-session', sessionArgs(e.sessionId, await transcriptOf($, e.sessionId)))
    return ended
  })

  on('command.run', { command: 'fold' }, async ($, e, next) => {
    const direction = e.args.trim()
    if (folding) return { text: await handBack($, 'Not folded: a fold is under way.', direction) }
    const built = await build($, next.signal)
    return 'failed' in built ? { text: await handBack($, built.failed, direction) } : request($, direction)
  })

  on('turn.complete', async ($, e, next) => {
    const ended = await next(e)
    const asked = handoff
    if (e.agentId !== undefined || asked?.turnId === undefined || e.turnId !== asked.turnId) return ended
    handoff = undefined
    if (e.reason !== 'answer' || e.answer.trim() === '') {
      $.ui.log(await handBack($, `Not folded: ${UNWRITTEN[e.reason]}, so this session is as it was.`, asked.direction))
      return ended
    }
    const failed = await fold($, e.answer, asked.direction, next.signal)
    $.ui.log(failed === undefined ? 'Folding.' : await handBack($, failed, asked.direction))
    return ended
  })

  on('command.run', { command: 'reply' }, async ($, e) => {
    const failed = await quote($, e.args.trim())
    return failed === undefined ? {} : { text: failed }
  })

  on('command.run', { command: 'ai' }, async ($, e) => {
    const [sub, ...rest] = e.args.trim().split(/\s+/)
    if (sub === 'new' && rest.length === 1) {
      const made = await agent($, ['new', rest[0]!])
      if (made.exitCode !== 0) return { text: outcome(made) }
      return { text: `Created agent ${rest[0]}. ${await attach($, rest[0]!)}` }
    }
    if (sub === 'attach' && rest.length === 1) return { text: await attach($, rest[0]!) }
    if (sub === 'detach' && rest.length === 0) return { text: await detach($) }
    return { text: USAGE }
  })

  on('command.run', { command: 'recall' }, async ($, e) => {
    const direction = e.args.trim()
    return direction === '' ? { text: RECALL_USAGE } : recall($, direction)
  })

  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) {
      if (handoff !== undefined && handoff.turnId === undefined && e.text === handoff.request) handoff.turnId = e.turnId
      return next(e)
    }
    const state = await recalled($)
    if (state.prompts.includes(e.text)) await $.state.set(recalls, known(state, e.text, e.agentId))
    return next(e)
  })

  on('tool.call', async ($, e, next) => {
    if (e.agentId === undefined || !(await recalled($)).forks.includes(e.agentId)) return next(e)
    if (READERS.has(e.tool) || (e.tool === 'Bash' && isRead(e.command))) return next(e)
    return { deny: REFUSED }
  }).catch(($, e, next) => (next.called ? next(e) : { deny: 'ai: the recall guard failed, so it refuses.' }))
}
