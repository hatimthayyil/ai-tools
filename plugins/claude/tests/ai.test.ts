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

function run($: Engine, command: string, args: string) {
  return $.command.run({
    command,
    args,
    origin: { kind: 'composer' },
    presentation: { isFullscreen: false, columns: 80 },
  })
}

const ai = ($: Engine, args: string) => run($, 'ai', args)
const fold = ($: Engine, args = '') => run($, 'fold', args)
const recall = ($: Engine, args: string) => run($, 'recall', args)

function engine(on: On, answer: (args: readonly string[]) => Answer) {
  const clock = mock.clock(on)
  const calls: (readonly string[])[] = []
  const spawns: (readonly string[])[] = []
  const killed: (readonly string[])[] = []
  const spawning = { hangs: false }
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
  on('process.spawn', async function* (_$, e, next) {
    calls.push(e.argv)
    spawns.push(e.argv)
    if (spawning.hangs) {
      await new Promise<void>(stopped => next.signal.addEventListener('abort', stopped, { once: true }))
      killed.push(e.argv)
      return { value: { code: null, signal: 'SIGTERM' } }
    }
    const [code, stdout, stderr] = answer(e.argv.slice(1))
    if (stdout !== '') yield { stream: 'stdout' as const, text: stdout }
    if (stderr !== '') yield { stream: 'stderr' as const, text: stderr }
    return { value: { code, signal: null } }
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
  return { clock, calls, spawns, killed, spawning, compacts, logs }
}

function start($: Engine, source: 'startup' | 'resume' | 'clear' = 'startup') {
  return $.classic.SessionStart({ source, session_id: SESSION, transcript_path: TRANSCRIPT })
}

async function folded($: Engine, world: World) {
  const ran = fold($)
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

test('/fold, with only spaces after it, builds the view once the transcript is flushed, then folds once', async ($, on) => {
  const world = engine(on, views)
  await start($, 'resume')
  const ran = fold($, '  ')
  await world.clock.advance(FLUSH_MS - 1)
  expect(world.calls).toEqual([])
  await world.clock.advance(1)
  expect((await ran).text).toBe('Folding.')
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'view', '--whole', ...ALL_ARGS],
  ])
  expect(world.spawns).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
  expect(world.compacts).toEqual(['/compact'])
  expect(await compact($)).toEqual([{ role: 'user', text: WOKE, toolUses: [] }])
  expect(await compact($)).toEqual(NATIVE)
  expect(world.compacts).toEqual(['/compact', 'manual'])
})

test(
  'cancelling /fold stops ai agent compact-session and folds nothing',
  {
    plugins: [
      {
        name: 'cancel',
        tier: 'prepend',
        register(on) {
          on('command.run', { command: 'fold' }, async ($, e, next) => {
            void next(e).catch(() => undefined)
            await $.clock.sleep(5_000)
            return { text: 'Cancelled.' }
          })
        },
      },
    ],
  },
  async ($, on) => {
    const world = engine(on, views)
    world.spawning.hangs = true
    await start($, 'resume')
    const ran = fold($)
    await world.clock.advance(FLUSH_MS)
    expect(world.spawns).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
    expect(world.killed).toEqual([])
    await world.clock.advance(5_000)
    expect((await ran).text).toBe('Cancelled.')
    await world.clock.advance(0)
    expect(world.killed).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
    expect(world.calls).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
    expect(world.compacts).toEqual([])
    expect(await compact($)).toEqual(NATIVE)
  },
)

test('/compact and auto-compaction stay Claude Code’s, without ai agent', async ($, on) => {
  const world = engine(on, () => [0, VIEW, ''])
  await start($, 'resume')
  expect(await compact($, 'manual')).toEqual(NATIVE)
  expect(await compact($, 'auto')).toEqual(NATIVE)
  expect(world.compacts).toEqual(['manual', 'auto'])
  expect(world.calls).toEqual([])
})

test('/fold in an unattached session folds into the memory alone', async ($, on) => {
  const MEMORY = '# Memory\n\nYou are awake.\n'
  const world = engine(on, args => (sub(args) === 'view' ? [0, MEMORY, ''] : [3, '', '']))
  await start($, 'resume')
  expect(await folded($, world)).toBe('Folding.')
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'view', '--whole', ...ALL_ARGS],
  ])
  expect(world.compacts).toEqual(['/compact'])
  expect(await compact($)).toEqual([{ role: 'user', text: MEMORY, toolUses: [] }])
  expect(world.logs).toEqual([])
})

test('/fold folds nothing and says why when ai fails or the thread is empty', async ($, on) => {
  let answer: Answer = [1, '', 'Failed 12: empty reply\n']
  const world = engine(on, args => (sub(args) === 'view' ? [1, '', 'memory locked\n'] : answer))
  await start($, 'resume')
  expect(await folded($, world)).toBe('Not folded: Failed 12: empty reply')
  answer = [3, '', '']
  expect(await folded($, world)).toBe('Not folded: memory locked')
  answer = [0, '', '']
  expect(await folded($, world)).toBe('Not folded: the thread holds nothing yet.')
  answer = [0, VIEW, '']
  expect(await folded($, world)).toBe('Not folded: memory locked')
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

test('a subagent compacts natively, leaving the view /fold built for the main conversation', async ($, on) => {
  const world = engine(on, views)
  await start($, 'resume')
  const ran = fold($)
  await world.clock.advance(FLUSH_MS)
  expect((await ran).text).toBe('Folding.')
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
  expect((await ai($, 'fold')).text).toMatch(/^Usage/)
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'attach', 'dev', ...ALL_ARGS],
    ['ai', 'agent', 'detach', ...ALL_ARGS],
  ])
})

test('/ai alone prints its usage, /fold with words after it says it takes none, and neither folds', async ($, on) => {
  const world = engine(on, views)
  await start($, 'resume')
  expect((await ai($, '')).text).toBe('Usage: /ai new <name> | /ai attach <name> | /ai detach')
  expect((await ai($, '  ')).text).toMatch(/^Usage: \/ai new/)
  expect((await fold($, 'now')).text).toBe('/fold takes no arguments.')
  await world.clock.advance(FLUSH_MS)
  expect(world.calls).toEqual([])
  expect(world.compacts).toEqual([])
  expect(await compact($)).toEqual(NATIVE)
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

const RECALL = 'Recall.\n\n{direction}\n'
const FORK = 'r1'
const RECALLING = 'Recalling. The brief follows when the fork ends.'

type Spawn = (prompt: string) => Promise<{ model: string } | { deny: string }>

/**
 * Stubs the engine under a recall. `spawn` answers the fork's spawn. The kit's engine starts no
 * agent and keeps no id from a hook's answer, so here the guard learns the fork only from its
 * turn's start.
 */
function recalling(on: On, spawn: Spawn = async () => ({ model: 'm' })) {
  const reads: string[] = []
  const prompts: { prompt: string }[] = []
  const passed: string[] = []
  on('fs.read', async (_$, e) => {
    reads.push(e.path)
    return { value: RECALL }
  })
  on('agent.spawn', async (_$, e) => {
    prompts.push(e)
    return spawn(e.prompt)
  })
  on('turn.start', async (_$, e) => ({ turnId: e.turnId }))
  on('tool.call', async (_$, e) => {
    passed.push(e.tool === 'Bash' ? e.command : e.tool)
    return { result: 'ok' }
  })
  return { reads, prompts, passed }
}

function begin($: Engine, text: string, agentId: string) {
  return $.turn.start({ text, turnId: `t-${agentId}`, agentId })
}

function call($: Engine, tool: string, input: Record<string, string>, agentId = FORK) {
  return $.tool.call({ tool, ...input, agentId } as Parameters<typeof $.tool.call>[0])
}

test('/recall starts a fork on the message and tells the model how to relay its brief', async ($, on) => {
  const world = engine(on, views)
  const got = recalling(on)
  const message = 'I want to resume  work on ai task.\nPerhaps a compiler, $& and all.'
  const ran = await recall($, ` ${message} `)
  expect(ran.text).toBe(RECALLING)
  expect(ran.context).toHaveLength(1)
  expect(ran.context![0]).toMatch(/^\/recall started a read-only fork/)
  expect(ran.context![0]).not.toContain(message)
  expect(got.reads).toHaveLength(1)
  expect(got.reads[0]).toMatch(/\/prompts\/recall\.txt$/)
  // The kit's engine hands `agent.spawn` the Agent tool's input for it.
  expect(got.prompts).toEqual([
    {
      tool: 'Agent',
      prompt: `Recall.\n\n${message}\n`,
      description: 'Recall',
      run_in_background: true,
      subagent_type: 'fork',
    },
  ])
  expect(world.calls).toEqual([])
})

test('/recall with no message prints its usage and starts nothing', async ($, on) => {
  const got = recalling(on)
  expect((await recall($, '')).text).toBe('Usage: /recall <message>')
  expect((await recall($, '   ')).text).toBe('Usage: /recall <message>')
  expect(got.prompts).toEqual([])
})

test('a recall the engine refuses says why, and leaves nothing to guard', async ($, on) => {
  const got = recalling(on, async () => ({ deny: 'past a Stop' }))
  expect((await recall($, 'the compiler')).text).toBe('Not recalling: past a Stop')
  await begin($, got.prompts[0]!.prompt, FORK)
  expect((await call($, 'Bash', { command: 'ai memory note x' })).deny).toBeUndefined()
})

test('a recall fork reads, and every tool call that could write is refused', async ($, on) => {
  const got = recalling(on)
  await recall($, 'the compiler')
  await begin($, got.prompts[0]!.prompt, FORK)
  await begin($, 'Another task.', 'other')
  const reads = [
    "ai memory grep 'TaskJuggler|compiler'",
    'ai memory zoom 256+8',
    'ai agent zoom 464 --tool-output',
    'ai page 1:2',
    'git log --oneline -5',
    'but status',
    'ai agent grep "task compiler"',
  ]
  for (const command of reads) expect([command, (await call($, 'Bash', { command })).deny]).toEqual([command, undefined])
  expect((await call($, 'Read', { file_path: '/r/ai/backlog.md' })).deny).toBeUndefined()
  const writes = [
    'ai memory note "a note"',
    'ai memory view; ai memory note x',
    'ai memory view && touch x',
    'ai agent zoom 3 > out',
    'ai memory grep "$(rm x)"',
    'ai memory grep `rm x`',
    "ai memory grep 'x",
    'git diff --output=x',
    'git branch x',
    'ai memory tree use 1',
    'echo x',
    'ai memory view\nai memory note x',
    'ai memory grep foo*',
    'ai memory grep \\x',
    'ai memory view # x',
  ]
  for (const command of writes) {
    expect(`${command} → ${(await call($, 'Bash', { command })).deny}`).toMatch(/ → A recall only reads/)
  }
  const edit = { file_path: '/r/x', old_string: 'a', new_string: 'b' }
  expect((await call($, 'Edit', edit)).deny).toMatch(/^A recall only reads/)
  expect((await call($, 'Write', { file_path: '/r/x', content: 'b' })).deny).toMatch(/^A recall only reads/)
  expect((await call($, 'Agent', { prompt: 'p', description: 'd' })).deny).toMatch(/^A recall only reads/)
  expect((await call($, 'mcp__github__create_issue', { title: 't' })).deny).toMatch(/^A recall only reads/)
  expect((await call($, 'Bash', { command: 'ai memory note x' }, 'other')).deny).toBeUndefined()
  expect((await $.tool.call({ tool: 'Bash', command: 'ai memory note y' })).deny).toBeUndefined()
  expect(got.passed).toEqual([...reads, 'Read', 'ai memory note x', 'ai memory note y'])
})

test('a recall fork whose start comes before the spawn answers is guarded', async ($, on) => {
  const got = recalling(on, async prompt => {
    await begin($, prompt, FORK)
    return { model: 'm' }
  })
  expect((await recall($, 'the compiler')).text).toBe(RECALLING)
  expect((await call($, 'Bash', { command: 'ai memory note x' })).deny).toMatch(/^A recall only reads/)
  expect(got.passed).toEqual([])
})

test('/fold folds nothing and says why when the plugin never saw the session start', async ($, on) => {
  const world = engine(on, views)
  expect((await fold($)).text).toMatch(/^Not folded: the ai plugin never saw this session start/)
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

test('an ai agent command that goes through shows what it warned of', async ($, on) => {
  const gone = 'Thread dev: /t/s-1.jsonl is gone; this thread read 8B of it.'
  const world = engine(on, args => (sub(args) === 'session-name' ? [0, 'repo.dev\n', ''] : [0, '', `${gone}\n`]))
  await stop($)
  await world.clock.advance(FLUSH_MS)
  await $.session.end({ reason: 'other', sessionId: SESSION, resume: { id: SESSION } })
  expect(world.logs).toEqual([gone, gone])
  await ai($, 'attach dev')
  expect(world.logs).toEqual([gone, gone, gone])
})

test('a session syncs and commits as it ends, and only a real failure is shown', async ($, on) => {
  let answer: Answer = [0, '', '']
  const world = engine(on, () => answer)
  await start($, 'resume')
  await $.session.end({ reason: 'prompt_input_exit', sessionId: SESSION, resume: { id: SESSION } })
  answer = [3, '', '']
  await $.session.end({ reason: 'clear', sessionId: SESSION, resume: { id: SESSION } })
  answer = [1, '', 'Cannot commit .ai: locked\n']
  await $.session.end({ reason: 'other', sessionId: SESSION, resume: { id: SESSION } })
  expect(world.calls).toEqual([
    ['ai', 'agent', 'end-session', ...ALL_ARGS],
    ['ai', 'agent', 'end-session', ...ALL_ARGS],
    ['ai', 'agent', 'end-session', ...ALL_ARGS],
  ])
  expect(world.logs).toEqual(['ai agent end-session failed: Cannot commit .ai: locked'])
})
