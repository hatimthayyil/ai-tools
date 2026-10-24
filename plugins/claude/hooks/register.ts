import type { EngineInterface, Register } from 'claude-code'

const NOT_ATTACHED = 3
const LONG_MS = 600_000
const FLUSH_MS = 1_100
const USAGE = 'Usage: /ai | /ai attach <name> | /ai detach'
const WITHHELD =
  "Not compacted: this session's start never reached the ai plugin, so its compaction would not either and /compact would summarise natively. Where an organization manages Claude Code, only a plugin its managed settings install receives these events."

const transcript = { plugin: 'ai', key: 'transcript' } as const
const prepared = { plugin: 'ai', key: 'view' } as const
const requested = { plugin: 'ai', key: 'compress' } as const

type Ran = { exitCode: number; stdout: string; stderr: string }

async function ai($: EngineInterface, args: string[], timeoutMs?: number): Promise<Ran> {
  try {
    return await $.process.run(['ai', ...args], { timeoutMs })
  } catch (err) {
    return { exitCode: -1, stdout: '', stderr: String(err) }
  }
}

function agent($: EngineInterface, args: string[], timeoutMs?: number): Promise<Ran> {
  return ai($, ['agent', ...args], timeoutMs)
}

function sessionArgs(id: string, path: string | undefined): string[] {
  return ['--harness', 'claude', '--session', id, ...(path === undefined ? [] : ['--transcript', path])]
}

async function session($: EngineInterface): Promise<string[]> {
  return sessionArgs(await $.session.id(), (await $.state.get(transcript)).value)
}

async function view($: EngineInterface, args: string[], fallback: string): Promise<string | undefined> {
  const ran = await agent($, ['compact-session', ...args], LONG_MS)
  if (ran.exitCode === 0 && ran.stdout !== '') return ran.stdout
  if (ran.exitCode !== 0 && ran.exitCode !== NOT_ATTACHED) {
    $.ui.log(`ai agent compact-session failed; ${fallback}. ${ran.stderr.trim()}`)
  }
  return undefined
}

async function sync($: EngineInterface, args: string[]): Promise<void> {
  const ran = await agent($, ['sync-session', ...args])
  if (ran.exitCode !== 0 && ran.exitCode !== NOT_ATTACHED) $.ui.log(`ai agent sync-session failed: ${ran.stderr.trim()}`)
}

function reply(ran: Ran): string {
  if (ran.exitCode === 0) return ran.stdout.trim()
  if (ran.exitCode === NOT_ATTACHED) return 'This session is not attached to an agent. Attach it with /ai attach <name>.'
  return ran.stderr.trim() || `ai agent exited with code ${ran.exitCode}.`
}

async function isWithheld($: EngineInterface): Promise<boolean> {
  return (await $.state.get(transcript)).value === undefined
}

async function compress($: EngineInterface): Promise<string | undefined> {
  if (await isWithheld($)) return WITHHELD
  await $.clock.sleep(FLUSH_MS)
  const args = await session($)
  const ready = await agent($, ['compact-session', ...args], LONG_MS)
  if (ready.exitCode === NOT_ATTACHED) return reply(ready)
  if (ready.exitCode !== 0) return `Not compacted: ${reply(ready)}`
  if (ready.stdout === '') return 'Not compacted: the thread holds nothing yet.'
  const shown = await ai($, ['view', '--whole', ...args])
  if (shown.exitCode !== 0) return `Not compacted: ${reply(shown)}`
  await $.state.set(prepared, shown.stdout)
  $.clock.after(0, () => $.command.run({ command: 'compact' }))
  return undefined
}

async function compressNow($: EngineInterface): Promise<void> {
  const failed = await compress($)
  if (failed !== undefined) $.ui.log(failed)
}

export const register: Register = on => {
  on('session.start', async ($, e, next) => {
    await $.command.register({
      name: 'ai',
      description: "ai: compact into your memory and the thread's view; attach <name>, detach",
      argumentHint: '[attach <name> | detach]',
    })
    await $.tool.register({
      name: 'compress',
      description:
        "Compact this session into your memory and its thread's view when the current turn ends. Works only in a session attached to an agent.",
      inputSchema: { type: 'object', properties: {} },
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
    if ((await $.state.get(requested)).value) {
      await $.state.set(requested, false)
      $.clock.after(0, () => compressNow($))
      return next(e)
    }
    const args = sessionArgs(e.session_id, e.transcript_path)
    $.clock.after(FLUSH_MS, () => sync($, args))
    return next(e)
  })

  on('session.end', async ($, e, next) => {
    const ended = await next(e)
    await sync($, sessionArgs(e.sessionId, (await $.state.get(transcript)).value))
    return ended
  })

  on('command.run', { command: 'ai' }, async ($, e) => {
    const [sub, ...rest] = e.args.trim().split(/\s+/)
    if (sub === '' && rest.length === 0) return { text: (await compress($)) ?? 'Compacting.' }
    if (sub === 'attach' && rest.length === 1) {
      return { text: reply(await agent($, ['attach', rest[0]!, ...(await session($))], LONG_MS)) }
    }
    if (sub === 'detach' && rest.length === 0) {
      return { text: reply(await agent($, ['detach', ...(await session($))])) }
    }
    return { text: USAGE }
  })

  on('tool.call', { tool: 'mcp__ai__compress' }, async $ => {
    if (await isWithheld($)) return { result: WITHHELD }
    const ran = await agent($, ['sync-session', ...(await session($))])
    if (ran.exitCode !== 0) return { result: reply(ran) }
    await $.state.set(requested, true)
    return { result: 'Compaction queued for the end of this turn.' }
  })
}
