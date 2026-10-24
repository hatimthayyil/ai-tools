import type { On, SessionCompactInput, SessionMessage } from 'claude-code'
import { expect, mock, test } from 'claude-code/testing'
import type { Engine } from 'claude-code/testing'

const SESSION = 's1'
const TRANSCRIPT = '/t/s1.jsonl'
const VIEW = '<view>KIWI-9</view>\n'
const WOKE = `# Memory\n\nYou are awake.\n\n${VIEW}`
const NATIVE: SessionMessage[] = [{ role: 'user', text: 'native summary', toolUses: [] }]
const PROJECT = 'a project hook context'
const SESSION_ARGS = ['--harness', 'claude', '--session', SESSION]
const ALL_ARGS = [...SESSION_ARGS, '--transcript', TRANSCRIPT]
const FLUSH_MS = 1_100

type Answer = [exitCode: number, stdout: string, stderr: string]
type World = ReturnType<typeof engine>

const sub = (args: readonly string[]) => (args[0] === 'agent' ? args[1] : args[0])
const views = (args: readonly string[]): Answer => [0, sub(args) === 'view' ? WOKE : VIEW, '']

function ai($: Engine, args: string) {
  return $.command.run({
    command: 'ai',
    args,
    origin: { kind: 'composer' },
    presentation: { isFullscreen: false, columns: 80 },
  })
}

function engine(on: On, answer: (args: readonly string[]) => Answer) {
  const clock = mock.clock(on)
  const calls: (readonly string[])[] = []
  const compacts: string[] = []
  const logs: string[] = []
  on('session.id', async () => ({ value: SESSION }))
  on('classic.SessionStart', async () => ({ additionalContext: [PROJECT] }))
  on('classic.Stop', async () => ({}))
  on('session.end', async (_$, e) => ({ sessionId: e.sessionId }))
  on('process.run', async (_$, e) => {
    calls.push(e.argv)
    const [exitCode, stdout, stderr] = answer(e.argv.slice(1))
    return { value: { exitCode, stdout, stderr, isStdoutTruncated: false, isStderrTruncated: false } }
  })
  on('session.compact', async (_$, e) => {
    compacts.push(e.trigger)
    return { messages: NATIVE }
  })
  on('command.run', { command: 'compact' }, async () => {
    compacts.push('/compact')
    return {}
  })
  on('ui.log', async (_$, e) => {
    logs.push(e.text)
    return { value: undefined }
  })
  return { clock, calls, compacts, logs }
}

function start($: Engine, source: 'startup' | 'resume' | 'clear' = 'startup') {
  return $.classic.SessionStart({ source, session_id: SESSION, transcript_path: TRANSCRIPT })
}

async function compress($: Engine, world: World) {
  const ran = ai($, '')
  await world.clock.advance(FLUSH_MS)
  const { text } = await ran
  await world.clock.advance(0)
  return text
}

function compact($: Engine, trigger: SessionCompactInput['trigger'] = 'manual') {
  return $.session.compact({ trigger, messages: [] }).then(r => r.messages)
}

function stop($: Engine) {
  return $.classic.Stop({ session_id: SESSION, transcript_path: TRANSCRIPT, stop_hook_active: false })
}

test('/ai builds the view once the transcript is flushed, then compacts into memory and view once', async ($, on) => {
  const world = engine(on, views)
  await start($, 'resume')
  const ran = ai($, '')
  await world.clock.advance(FLUSH_MS - 1)
  expect(world.calls).toEqual([])
  await world.clock.advance(1)
  expect((await ran).text).toBe('Compacting.')
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'view', '--whole', ...ALL_ARGS],
  ])
  expect(world.compacts).toEqual(['/compact'])
  expect(await compact($)).toEqual([{ role: 'user', text: WOKE, toolUses: [] }])
  expect(await compact($)).toEqual(NATIVE)
  expect(world.compacts).toEqual(['/compact', 'manual'])
})

test('/compact and auto-compaction stay Claude Code’s, without ai agent', async ($, on) => {
  const world = engine(on, () => [0, VIEW, ''])
  await start($, 'resume')
  expect(await compact($, 'manual')).toEqual(NATIVE)
  expect(await compact($, 'auto')).toEqual(NATIVE)
  expect(world.compacts).toEqual(['manual', 'auto'])
  expect(world.calls).toEqual([])
})

test('/ai in an unattached session compacts into the memory alone', async ($, on) => {
  const MEMORY = '# Memory\n\nYou are awake.\n'
  const world = engine(on, args => (sub(args) === 'view' ? [0, MEMORY, ''] : [3, '', '']))
  await start($, 'resume')
  expect(await compress($, world)).toBe('Compacting.')
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'view', '--whole', ...ALL_ARGS],
  ])
  expect(world.compacts).toEqual(['/compact'])
  expect(await compact($)).toEqual([{ role: 'user', text: MEMORY, toolUses: [] }])
  expect(world.logs).toEqual([])
})

test('/ai compacts nothing and says why when ai fails or the thread is empty', async ($, on) => {
  let answer: Answer = [1, '', 'Failed 12: empty reply\n']
  const world = engine(on, args => (sub(args) === 'view' ? [1, '', 'memory locked\n'] : answer))
  await start($, 'resume')
  expect(await compress($, world)).toBe('Not compacted: Failed 12: empty reply')
  answer = [3, '', '']
  expect(await compress($, world)).toBe('Not compacted: memory locked')
  answer = [0, '', '']
  expect(await compress($, world)).toBe('Not compacted: the thread holds nothing yet.')
  answer = [0, VIEW, '']
  expect(await compress($, world)).toBe('Not compacted: memory locked')
  expect(world.compacts).toEqual([])
  expect(await compact($)).toEqual(NATIVE)
  expect(world.logs).toEqual([])
})

test('a new session of a thread starts with its view', async ($, on) => {
  const world = engine(on, () => [0, VIEW, ''])
  const result = await start($)
  expect(result.additionalContext).toEqual([PROJECT, VIEW])
  expect(world.calls).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
})

test('a resumed, cleared, unattached or empty session starts as it would', async ($, on) => {
  let answer: Answer = [0, VIEW, '']
  const world = engine(on, () => answer)
  expect((await start($, 'resume')).additionalContext).toEqual([PROJECT])
  expect((await start($, 'clear')).additionalContext).toEqual([PROJECT])
  expect(world.calls).toEqual([])
  answer = [3, '', '']
  expect((await start($)).additionalContext).toEqual([PROJECT])
  answer = [0, '', '']
  expect((await start($)).additionalContext).toEqual([PROJECT])
  expect(world.calls.length).toBe(2)
  expect(world.logs).toEqual([])
})

test('a failed seed starts without the view and says why', async ($, on) => {
  const world = engine(on, () => [1, '', 'no claude on PATH\n'])
  expect((await start($)).additionalContext).toEqual([PROJECT])
  expect(world.logs).toEqual(["ai agent compact-session failed; starting without the thread's view. no claude on PATH"])
})

test('a subagent compacts natively, leaving the view /ai built for the main conversation', async ($, on) => {
  const world = engine(on, views)
  await start($, 'resume')
  const ran = ai($, '')
  await world.clock.advance(FLUSH_MS)
  expect((await ran).text).toBe('Compacting.')
  await $.session.compact({ trigger: 'auto', agentId: 'a1', messages: [] })
  expect(world.compacts).toEqual(['/compact', 'auto'])
  expect(await compact($)).toEqual([{ role: 'user', text: WOKE, toolUses: [] }])
})

test('/ai attach and detach run ai agent with the session and show its answer', async ($, on) => {
  const world = engine(on, args =>
    sub(args) === 'attach' ? [0, 'Attached to dev.\n', ''] : [1, '', 'This session is not attached to an agent.\n'],
  )
  await start($)
  expect((await ai($, 'attach dev')).text).toBe('Attached to dev.')
  expect((await ai($, ' detach ')).text).toBe('This session is not attached to an agent.')
  expect((await ai($, 'attach')).text).toMatch(/^Usage/)
  expect((await ai($, 'compress')).text).toMatch(/^Usage/)
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'attach', 'dev', ...ALL_ARGS],
    ['ai', 'agent', 'detach', ...ALL_ARGS],
  ])
})

test('/ai new creates the agent, then attaches the session to it; a failed new attaches nothing', async ($, on) => {
  const world = engine(on, args =>
    args[2] === 'taken'
      ? [1, '', 'Agent taken already exists in this project.\n']
      : sub(args) === 'new'
        ? [0, 'Created agent dev. Start it: ai agent resume dev\n', '']
        : [0, 'Attached to dev.\n', ''],
  )
  await start($)
  expect((await ai($, 'new dev')).text).toBe('Created agent dev. Attached to dev.')
  expect((await ai($, 'new taken')).text).toBe('Agent taken already exists in this project.')
  expect((await ai($, 'new')).text).toMatch(/^Usage/)
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'new', 'dev'],
    ['ai', 'agent', 'attach', 'dev', ...ALL_ARGS],
    ['ai', 'agent', 'new', 'taken'],
  ])
})

async function turnEnd($: Engine, world: World) {
  await stop($)
  await world.clock.advance(0)
  await world.clock.advance(FLUSH_MS)
  await world.clock.advance(0)
}

test('the agent tool compacts as /ai does at the end of the turn', async ($, on) => {
  const world = engine(on, args => (sub(args) === 'sync-session' ? [0, '', ''] : views(args)))
  const call = () => $.tool.call({ tool: 'mcp__ai__compress', input: {} })
  await start($, 'resume')
  expect(await call()).toMatchObject({ result: 'Compaction queued for the end of this turn.' })
  expect(world.compacts).toEqual([])
  await turnEnd($, world)
  expect(world.compacts).toEqual(['/compact'])
  expect(await compact($)).toEqual([{ role: 'user', text: WOKE, toolUses: [] }])
  await turnEnd($, world)
  expect(world.compacts).toEqual(['/compact'])
  expect(world.calls).toEqual([
    ['ai', 'agent', 'sync-session', ...ALL_ARGS],
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'view', '--whole', ...ALL_ARGS],
    ['ai', 'agent', 'sync-session', ...ALL_ARGS],
  ])
  expect(world.logs).toEqual([])
})

test('the agent tool in an unattached session compacts into the memory alone', async ($, on) => {
  const MEMORY = '# Memory\n\nYou are awake.\n'
  const world = engine(on, args => (sub(args) === 'view' ? [0, MEMORY, ''] : [3, '', '']))
  await start($, 'resume')
  expect(await $.tool.call({ tool: 'mcp__ai__compress', input: {} })).toMatchObject({
    result: 'Compaction queued for the end of this turn.',
  })
  await turnEnd($, world)
  expect(world.compacts).toEqual(['/compact'])
  expect(await compact($)).toEqual([{ role: 'user', text: MEMORY, toolUses: [] }])
  expect(world.logs).toEqual([])
})

test('a compaction the agent asked for that fails says why and compacts nothing', async ($, on) => {
  const world = engine(on, args => (sub(args) === 'compact-session' ? [1, '', 'Failed 12: empty reply\n'] : [0, '', '']))
  await start($, 'resume')
  await $.tool.call({ tool: 'mcp__ai__compress', input: {} })
  await turnEnd($, world)
  expect(world.compacts).toEqual([])
  expect(world.logs).toEqual(['Not compacted: Failed 12: empty reply'])
})

test('/ai and the agent tool compact nothing and say why when the session’s start never reached the plugin', async ($, on) => {
  const world = engine(on, views)
  const text = (await ai($, '')).text
  expect(text).toMatch(/^Not compacted: this session's start never reached the ai plugin/)
  expect(await $.tool.call({ tool: 'mcp__ai__compress', input: {} })).toMatchObject({ result: text })
  await world.clock.advance(FLUSH_MS)
  expect(world.calls).toEqual([])
  expect(world.compacts).toEqual([])
  expect(await compact($)).toEqual(NATIVE)
})

test('turn end syncs once the transcript is flushed, and only a real failure is shown', async ($, on) => {
  let answer: Answer = [3, '', '']
  const world = engine(on, () => answer)
  await stop($)
  await world.clock.advance(FLUSH_MS - 1)
  expect(world.calls).toEqual([])
  await world.clock.advance(1)
  answer = [1, '', 'store locked\n']
  await stop($)
  await world.clock.advance(FLUSH_MS)
  expect(world.calls).toEqual([
    ['ai', 'agent', 'sync-session', ...ALL_ARGS],
    ['ai', 'agent', 'sync-session', ...ALL_ARGS],
  ])
  expect(world.logs).toEqual(['ai agent sync-session failed: store locked'])
})

test('a session syncs as it ends', async ($, on) => {
  const world = engine(on, () => [0, '', ''])
  await start($, 'resume')
  await $.session.end({ reason: 'prompt_input_exit', sessionId: SESSION, resume: { id: SESSION } })
  expect(world.calls).toEqual([['ai', 'agent', 'sync-session', ...ALL_ARGS]])
})
