import type { CommandRunResult, EngineInterface, Register } from 'claude-code'
import type { AiRecall } from '../types'

const NOT_ATTACHED = 3
const LONG_MS = 600_000
const FLUSH_MS = 1_100
const USAGE = 'Usage: /ai new <name> | /ai attach <name> | /ai detach'
const FOLD_USAGE = '/fold takes no arguments.'
const WITHHELD =
  'Not folded: the ai plugin never saw this session start, so it would not see the compaction either, and Claude Code would write its own summary. Where an organization manages Claude Code, only plugins its managed settings install see these events.'
const RECALL_USAGE = 'Usage: /recall <message>'
const RELAY =
  "/recall started a read-only fork on the user's message. When its brief arrives, show it to the user whole and wait for their review. Then write the whole brief again with their corrections, and note nothing from it they have not approved."
const REFUSED =
  'A recall only reads: Read, Grep, Glob, and one read-only ai, git or but command per Bash call, with no pipes, redirections or substitutions (quote patterns in single quotes).'

const transcript = { plugin: 'ai', key: 'transcript' } as const
const prepared = { plugin: 'ai', key: 'view' } as const
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

async function session($: EngineInterface): Promise<string[]> {
  return sessionArgs(await $.session.id(), (await $.state.get(transcript)).value)
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

function reply(ran: Ran): string {
  if (ran.exitCode === 0) return ran.stdout.trim()
  if (ran.exitCode === NOT_ATTACHED) return 'This session is not attached to an agent. Attach it with /ai attach <name>.'
  return ran.stderr.trim() || `ai agent exited with code ${ran.exitCode}.`
}

async function attach($: EngineInterface, name: string): Promise<string> {
  return reply(await agent($, ['attach', name, ...(await session($))], LONG_MS))
}

async function isWithheld($: EngineInterface): Promise<boolean> {
  return (await $.state.get(transcript)).value === undefined
}

/**
 * Folds the session: builds the thread's view when the session is attached, prints `ai view`
 * (the memory's view, then the thread's), and runs `/compact`, which `session.compact` answers
 * with that text. Returns why it folds nothing, else undefined.
 */
async function fold($: EngineInterface, cancelled?: AbortSignal): Promise<string | undefined> {
  if (await isWithheld($)) return WITHHELD
  await $.clock.sleep(FLUSH_MS)
  const args = await session($)
  const ready = await compactSession($, args, cancelled)
  if (cancelled?.aborted) return 'Not folded: cancelled.'
  if (ready.exitCode !== 0 && ready.exitCode !== NOT_ATTACHED) return `Not folded: ${reply(ready)}`
  if (ready.exitCode === 0 && ready.stdout === '') return 'Not folded: the thread holds nothing yet.'
  const shown = await ai($, ['view', '--whole', ...args])
  if (shown.exitCode !== 0) return `Not folded: ${reply(shown)}`
  if (cancelled?.aborted) return 'Not folded: cancelled.'
  await $.state.set(prepared, shown.stdout)
  $.clock.after(0, () => $.command.run({ command: 'compact' }))
  return undefined
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

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'fold',
      description:
        "ai: replace this conversation with your memory's view, and the thread's view if this session is attached",
    })
    await $.command.register({
      name: 'ai',
      description:
        'ai: new <name> creates an agent and attaches this session to it; attach <name> attaches it to an existing agent; detach detaches it',
      argumentHint: 'new <name> | attach <name> | detach',
    })
    await $.command.register({
      name: 'recall',
      description:
        'ai: a read-only fork briefs you on your message from your memory, the thread and the repository',
      argumentHint: '<message>',
    })
    return next(e)
  })

  on('classic.SessionStart', async ($, e, next) => {
    await $.state.set(transcript, e.transcript_path)
    const started = await next(e)
    if (e.source !== 'startup') return started
    const seed = await view($, sessionArgs(e.session_id, e.transcript_path), "starting without the thread's view")
    if (seed === undefined) return started
    return { ...started, additionalContext: [...(started.additionalContext ?? []), seed] }
  })

  on('session.compact', async ($, e, next) => {
    if (e.agentId !== undefined) return next(e)
    const text = (await $.state.get(prepared)).value
    if (!text) return next(e)
    await $.state.set(prepared, '')
    return { messages: [{ role: 'user', text, toolUses: [] }] }
  })

  on('classic.Stop', async ($, e, next) => {
    const args = sessionArgs(e.session_id, e.transcript_path)
    $.clock.after(FLUSH_MS, () => sync($, 'sync-session', args))
    return next(e)
  })

  on('session.end', async ($, e, next) => {
    const ended = await next(e)
    await sync($, 'end-session', sessionArgs(e.sessionId, (await $.state.get(transcript)).value))
    return ended
  })

  on('command.run', { command: 'fold' }, async ($, e, next) => {
    if (e.args.trim() !== '') return { text: FOLD_USAGE }
    return { text: (await fold($, next.signal)) ?? 'Folding.' }
  })

  on('command.run', { command: 'ai' }, async ($, e) => {
    const [sub, ...rest] = e.args.trim().split(/\s+/)
    if (sub === 'new' && rest.length === 1) {
      const made = await agent($, ['new', rest[0]!])
      if (made.exitCode !== 0) return { text: reply(made) }
      return { text: `Created agent ${rest[0]}. ${await attach($, rest[0]!)}` }
    }
    if (sub === 'attach' && rest.length === 1) return { text: await attach($, rest[0]!) }
    if (sub === 'detach' && rest.length === 0) {
      return { text: reply(await agent($, ['detach', ...(await session($))])) }
    }
    return { text: USAGE }
  })

  on('command.run', { command: 'recall' }, async ($, e) => {
    const direction = e.args.trim()
    return direction === '' ? { text: RECALL_USAGE } : recall($, direction)
  })

  on('turn.start', async ($, e, next) => {
    if (e.agentId === undefined) return next(e)
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
