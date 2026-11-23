import type {
  CommandSpec,
  On,
  PromptFillResult,
  PromptOrigin,
  SessionCompactInput,
  SessionMessage,
  TurnCompleteReason,
} from 'claude-code'
import { expect, mock, test } from 'claude-code/testing'
import type { Engine, Plugin } from 'claude-code/testing'

const SESSION = 's1'
const TRANSCRIPT = '/t/s1.jsonl'
const VIEW = '<view>KIWI-9</view>\n'
const WOKE = `# Memory\n\nYou are awake.\n\n${VIEW}`
const NATIVE: SessionMessage[] = [{ role: 'user', text: 'native summary', toolUses: [] }]
const PROJECT = 'a project hook context'
const SESSION_ARGS = ['--harness', 'claude', '--session', SESSION]
const ALL_ARGS = [...SESSION_ARGS, '--transcript', TRANSCRIPT]
const FLUSH_MS = 1_100
const NEXT = 's2'
const UNATTACHED = 3
const INSTRUCTIONS = 'Write the handoff.\n'
const REQUEST = 'Fold this session.'
const ASKING = 'Writing a handoff. The fold follows it.'
const HANDOFF = 'Where things stand: KIWI-9.'
const HANDOFF_TURN = 'h1'
const OWN: PromptOrigin = { kind: 'plugin', name: 'ai', asUser: true }
/** The session the engine says this is: `engine` starts at `SESSION`, and a `/clear` that `clearing` answers moves on. */
const current = { id: SESSION }

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
const reply = ($: Engine, args = '') => run($, 'reply', args)
const recall = ($: Engine, args: string) => run($, 'recall', args)

/** The prompt box, holding `typed.text` as the draft, answering each `$.prompt.fill` with `answer`'s result. */
function box(on: On, answer: () => PromptFillResult = () => ({ isFilled: true }), typed = { text: '' }) {
  const fills: { text: string; mode: string }[] = []
  on('prompt.read', async () => ({ value: { text: typed.text, cursor: typed.text.length } }))
  on('prompt.fill', async (_$, e) => {
    fills.push({ text: e.text, mode: e.mode })
    return answer()
  })
  return fills
}

/** The mouse selection: each `$.ui.selection()` answers `text`, or nothing while it is undefined. */
function selecting(on: On) {
  const selection: { text?: string } = {}
  on('ui.selection', async () => ({ value: selection.text === undefined ? undefined : { text: selection.text } }))
  return selection
}

function engine(on: On, answer: (args: readonly string[]) => Answer) {
  const clock = mock.clock(on)
  const reads: string[] = []
  const submits: { text: string; context?: readonly string[]; origin: PromptOrigin }[] = []
  const submitting: { drop?: string } = {}
  const reading = { fails: false }
  on('fs.read', async (_$, e) => {
    reads.push(e.path)
    // The kit skips a hook that throws, so the read finds no implementation and rejects.
    if (reading.fails) throw new Error('unreadable')
    return { value: e.path.endsWith('/prompts/fold.txt') ? INSTRUCTIONS : RECALL }
  })
  on('prompt.submit', async (_$, e) => {
    submits.push({ text: e.text, context: e.context, origin: e.origin })
    return submitting.drop === undefined ? { text: e.text, context: e.context } : { drop: submitting.drop }
  })
  on('turn.start', async (_$, e) => ({ turnId: e.turnId }))
  on('turn.complete', async (_$, e) => ({ text: e.answer }))
  const calls: (readonly string[])[] = []
  const spawns: (readonly string[])[] = []
  const killed: (readonly string[])[] = []
  const spawning = { hangs: false }
  const compacts: string[] = []
  const logs: string[] = []
  current.id = SESSION
  on('session.id', async () => ({ value: current.id }))
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
      // As in the engine, a stream closed mid-read ends without a result.
      return { value: undefined as never }
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
  const compacting = { fails: false }
  on('command.run', { command: 'compact' }, async () => {
    // The kit skips a hook that throws, so the run finds no implementation and rejects.
    if (compacting.fails) throw new Error('refused')
    compacts.push('/compact')
    return {}
  })
  const renames: string[] = []
  const renaming = { fails: false }
  on('command.run', { command: 'rename' }, async (_$, e) => {
    // The kit skips a hook that throws, so the run finds no implementation and rejects.
    if (renaming.fails) throw new Error('refused')
    renames.push(e.args)
    return {}
  })
  on('ui.log', async (_$, e) => {
    logs.push(e.text)
    return { value: undefined }
  })
  return {
    clock,
    calls,
    spawns,
    killed,
    spawning,
    compacts,
    compacting,
    renames,
    renaming,
    logs,
    reads,
    reading,
    submits,
    submitting,
  }
}

/** `ai agent attach`, `detach` and `session-name` answering as for agent dev of project repo. */
const attaching = (args: readonly string[]): Answer =>
  sub(args) === 'attach'
    ? [0, 'Attached to dev.\n', '']
    : sub(args) === 'detach'
      ? [0, 'Detached from dev.\n', '']
      : sub(args) === 'session-name'
        ? [0, 'repo.dev\n', '']
        : [0, VIEW, '']

function start($: Engine, source: 'startup' | 'resume' | 'clear' = 'startup') {
  return $.classic.SessionStart({ source, session_id: SESSION, transcript_path: TRANSCRIPT })
}

/** A typed `/fold` with `args`, run while it builds the view once the transcript is flushed. */
async function ask($: Engine, world: World, args = '') {
  const ran = fold($, args)
  await world.clock.advance(FLUSH_MS)
  return ran
}

type Ended = { reason: TurnCompleteReason; answer: string }
const WROTE: Ended = { reason: 'answer', answer: HANDOFF }

/** The end of the handoff's turn as `end` says, which a fold that follows holds until it returns. */
function complete($: Engine, end: Ended) {
  const { reason, answer } = end
  const ended = { turnId: HANDOFF_TURN, durationMs: 1, isAborted: reason === 'aborted', answer }
  return reason === 'refusal'
    ? $.turn.complete({ ...ended, reason, refusal: { category: null, explanation: null } })
    : $.turn.complete({ ...ended, reason })
}

/**
 * The request for a handoff, `request`, going in, its turn starting, then ending as `end` says,
 * the clock run on through the fold that follows while it builds the view once the transcript
 * is flushed.
 */
async function handOff($: Engine, world: World, end: Ended = WROTE, request = REQUEST) {
  await world.clock.advance(0)
  await $.turn.start({ text: request, turnId: HANDOFF_TURN })
  const ended = complete($, end)
  await world.clock.advance(FLUSH_MS)
  await ended
}

/**
 * A typed `/fold` with `direction` run through its handoff: the request, its turn ending as
 * `end` says, then the fold that follows. Returns what the typed run answered when it asked for
 * no handoff, else what the turn's end logged, taken out of the logs.
 */
async function folded($: Engine, world: World, end: Ended = WROTE, direction = '') {
  const { text } = await ask($, world, direction)
  if (text !== ASKING) return text
  const before = world.logs.length
  const next = direction.trim()
  await handOff($, world, end, next === '' ? REQUEST : `${REQUEST} Next: ${next}`)
  const logged = world.logs.splice(before, 1)[0]
  await world.clock.advance(0)
  return logged
}

/** `ai` answering as for a session attached to agent dev: its views, and `carry-session` attaching the next one. */
const carried = (args: readonly string[]): Answer =>
  sub(args) === 'carry-session' ? [0, 'Attached to dev.\n', ''] : views(args)

/** `ai` answering as for a session attached to no agent: the memory's view alone. */
const unattached = (args: readonly string[]): Answer => (sub(args) === 'view' ? [0, WOKE, ''] : [UNATTACHED, '', ''])

/**
 * Answers `/clear` by starting session `clear.starts`, unless `clear.fails`, and a row the
 * plugin appends by storing it, unless `append.deny` refuses it. `order` has each clear and
 * stored row with how many `ai` calls came before it; `rows` has the stored rows' text.
 */
function clearing(on: On, world: World) {
  const clear = { fails: false, starts: NEXT }
  const append: { deny?: string } = {}
  const order: [what: string, calls: number][] = []
  const rows: string[] = []
  on('command.run', { command: 'clear' }, async () => {
    // The kit skips a hook that throws, so the run finds no implementation and rejects.
    if (clear.fails) throw new Error('refused')
    order.push(['/clear', world.calls.length])
    current.id = clear.starts
    return {}
  })
  on('session.append', { door: 'note' }, async (_$, e, next) => {
    if (append.deny !== undefined) return { deny: append.deny }
    order.push([`append ${e.message.type}`, world.calls.length])
    rows.push(e.message.content.map(block => (block.type === 'text' ? block.text : `[${block.type}]`)).join(''))
    return next(e)
  })
  return { clear, append, order, rows }
}

/** A `/fold` with `direction` run to its end: its answer, then the session it carries on in attached. */
async function carriedOn($: Engine, world: World, direction = '') {
  const text = await folded($, world, WROTE, direction)
  await world.clock.advance(FLUSH_MS)
  return text
}

function compact($: Engine, trigger: SessionCompactInput['trigger'] = 'manual') {
  return $.session.compact({ trigger, messages: [] }).then(r => r.messages)
}

function stop($: Engine) {
  return $.classic.Stop({ session_id: SESSION, transcript_path: TRANSCRIPT, stop_hook_active: false })
}

test('/fold builds the view once the transcript is flushed, leaves the model the instructions for a handoff, then asks for it in your words', async ($, on) => {
  const world = engine(on, carried)
  await start($, 'resume')
  const ran = fold($, '  ')
  await world.clock.advance(FLUSH_MS - 1)
  expect(world.calls).toEqual([])
  await world.clock.advance(1)
  const asked = await ran
  expect(asked.text).toBe(ASKING)
  expect(asked.context).toEqual([INSTRUCTIONS])
  expect(world.calls).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
  expect(world.reads).toHaveLength(1)
  expect(world.reads[0]).toMatch(/\/prompts\/fold\.txt$/)
  expect(world.submits).toEqual([{ text: REQUEST, context: undefined, origin: OWN }])
  await $.turn.start({ text: REQUEST, turnId: HANDOFF_TURN })
  await world.clock.advance(FLUSH_MS)
  expect(world.calls).toHaveLength(1)
  expect(world.logs).toEqual([])
})

test('/fold, once the handoff is written, builds the view again once the transcript is flushed, then goes on in a new session', async ($, on) => {
  const world = engine(on, carried)
  const cleared = clearing(on, world)
  await start($, 'resume')
  expect((await ask($, world)).text).toBe(ASKING)
  await world.clock.advance(0)
  await $.turn.start({ text: REQUEST, turnId: HANDOFF_TURN })
  const ended = complete($, WROTE)
  await world.clock.advance(FLUSH_MS - 1)
  expect(world.calls).toHaveLength(1)
  await world.clock.advance(1)
  expect((await ended).text).toBe(HANDOFF)
  expect(world.logs).toEqual(['Folding.'])
  expect(world.spawns).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
  ])
  await world.clock.advance(0)
  expect(cleared.order).toEqual([
    ['/clear', 3],
    ['append user', 3],
  ])
  expect(cleared.rows).toEqual([WOKE])
  expect(world.calls).toHaveLength(3)
  expect((await fold($)).text).toBe('Not folded: a fold is under way.')
  await world.clock.advance(FLUSH_MS)
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'view', '--whole', ...ALL_ARGS],
    ['ai', 'agent', 'carry-session', '--from', SESSION, '--harness', 'claude', '--session', NEXT],
  ])
  expect(world.logs).toEqual([
    'Folding.',
    "Folded into a new session. Attached to dev. It holds your memory's view and the thread's.",
  ])
  expect(world.submits).toHaveLength(1)
  expect(world.compacts).toEqual([])
  expect(await compact($)).toEqual(NATIVE)
})

test('/fold attaches the new session by its transcript once its start names it, and an ended session by its id alone', async ($, on) => {
  const world = engine(on, carried)
  clearing(on, world)
  await start($, 'resume')
  expect(await folded($, world)).toBe('Folding.')
  const next = { source: 'clear' as const, session_id: NEXT, transcript_path: '/t/s2.jsonl' }
  expect((await $.classic.SessionStart(next)).additionalContext).toEqual([PROJECT])
  await $.session.end({ reason: 'clear', sessionId: SESSION, resume: { id: SESSION } })
  await world.clock.advance(FLUSH_MS)
  const args = ['--harness', 'claude', '--session', NEXT, '--transcript', '/t/s2.jsonl']
  expect(world.calls.slice(3)).toEqual([
    ['ai', 'agent', 'end-session', ...SESSION_ARGS],
    ['ai', 'agent', 'carry-session', '--from', SESSION, ...args],
  ])
})

test('/fold in an attached session needs no session start: ai finds the transcripts by their ids', async ($, on) => {
  const world = engine(on, carried)
  const cleared = clearing(on, world)
  expect(await carriedOn($, world)).toBe('Folding.')
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...SESSION_ARGS],
    ['ai', 'agent', 'compact-session', ...SESSION_ARGS],
    ['ai', 'view', '--whole', ...SESSION_ARGS],
    ['ai', 'agent', 'carry-session', '--from', SESSION, '--harness', 'claude', '--session', NEXT],
  ])
  expect(cleared.rows).toEqual([WOKE])
})

test('/fold says what failed, and what the session holds: the clear, the view or the attach', async ($, on) => {
  let attach: Answer = [0, 'Attached to dev.\n', '']
  const world = engine(on, args => (sub(args) === 'carry-session' ? attach : views(args)))
  const cleared = clearing(on, world)
  await start($, 'resume')

  cleared.clear.fails = true
  expect(await carriedOn($, world)).toBe('Folding.')
  expect(world.logs.pop()).toMatch(/^Not folded: \/clear failed, so this session is as it was\. /)
  cleared.clear.fails = false
  cleared.clear.starts = SESSION
  expect(await carriedOn($, world)).toBe('Folding.')
  expect(world.logs.pop()).toBe('Not folded: /clear started no new session, so this session is as it was.')
  expect(cleared.rows).toEqual([])
  expect(world.calls.map(argv => sub(argv.slice(1)))).toEqual([
    'compact-session',
    'compact-session',
    'view',
    'compact-session',
    'compact-session',
    'view',
  ])

  cleared.clear.starts = NEXT
  cleared.append.deny = 'no notes here'
  expect(await carriedOn($, world)).toBe('Folding.')
  expect(world.logs.pop()).toBe(
    'Folded into a new session. Attached to dev. It holds no view, which was refused (no notes here): have Claude run ai view.',
  )
  expect(cleared.rows).toEqual([])

  cleared.append.deny = undefined
  cleared.clear.starts = 's3'
  attach = [1, '', 'Cannot lock the agents.\n']
  expect(await carriedOn($, world)).toBe('Folding.')
  expect(world.logs.pop()).toBe(
    "Folded into a new session, which is attached to no agent: ai agent carry-session failed. Cannot lock the agents. It holds your memory's view and the thread's. Attach it with /ai attach <name>.",
  )
  cleared.clear.starts = 's4'
  attach = [UNATTACHED, '', '']
  expect(await carriedOn($, world)).toBe('Folding.')
  expect(world.logs.pop()).toBe(
    "Folded into a new session, which is attached to no agent: ai agent carry-session failed. Session s3 is attached to none now. It holds your memory's view and the thread's. Attach it with /ai attach <name>.",
  )
  expect(cleared.rows).toEqual([WOKE, WOKE])
  expect(world.logs).toEqual([])
  expect(world.compacts).toEqual([])
})

/** A plugin above ai's that cancels a `/fold` 5 s in, as Esc does, and logs what ai's run beneath answered. */
const CANCELLING: Plugin = {
  name: 'cancel',
  tier: 'prepend',
  register(on) {
    on('command.run', { command: 'fold' }, async ($, e, next) => {
      void next(e).then(
        ({ text }) => $.ui.log(`beneath: ${text}`),
        (err: unknown) => $.ui.log(`beneath threw: ${String(err)}`),
      )
      await $.clock.sleep(5_000)
      return { text: 'Cancelled.' }
    })
  },
}

test('a /clear of your own carries nothing into the new session', async ($, on) => {
  const world = engine(on, carried)
  const cleared = clearing(on, world)
  await start($, 'resume')
  await run($, 'clear', '')
  const next = { source: 'clear' as const, session_id: NEXT, transcript_path: '/t/s2.jsonl' }
  expect((await $.classic.SessionStart(next)).additionalContext).toEqual([PROJECT])
  await world.clock.advance(FLUSH_MS)
  expect(cleared.order).toEqual([['/clear', 0]])
  expect(world.calls).toEqual([])
  expect(world.logs).toEqual([])
})

test(
  'cancelling /fold stops ai agent compact-session and asks for no handoff',
  { plugins: [CANCELLING] },
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
    expect(world.logs).toEqual(['beneath: Not folded: cancelled.'])
    expect(world.killed).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
    expect(world.calls).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
    expect(world.reads).toEqual([])
    expect(world.submits).toEqual([])
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

test('/fold in an unattached session compacts it into the memory and the handoff, once', async ($, on) => {
  const MEMORY = '# Memory\n\nYou are awake.\n'
  const world = engine(on, args => (sub(args) === 'view' ? [0, MEMORY, ''] : [3, '', '']))
  const cleared = clearing(on, world)
  await start($, 'resume')
  expect(await carriedOn($, world)).toBe('Folding.')
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'view', '--whole', ...ALL_ARGS],
  ])
  expect(world.compacts).toEqual(['/compact'])
  expect(cleared.order).toEqual([])
  expect(await compact($)).toEqual([
    { role: 'user', text: MEMORY, toolUses: [] },
    { role: 'assistant', text: HANDOFF, toolUses: [] },
  ])
  expect(await compact($)).toEqual(NATIVE)
  expect(world.compacts).toEqual(['/compact', 'manual'])
  expect(world.logs).toEqual([])
})

test('/fold folds nothing and says why when ai fails or the thread is empty; a build that fails first asks for no handoff', async ($, on) => {
  let answer: Answer = [1, '', 'Failed 12: empty reply\n']
  const world = engine(on, args => (sub(args) === 'view' ? [1, '', 'memory locked\n'] : answer))
  await start($, 'resume')
  expect(await folded($, world)).toBe('Not folded: Failed 12: empty reply')
  expect(world.submits).toEqual([])
  answer = [3, '', '']
  expect(await folded($, world)).toBe('Not folded: memory locked')
  answer = [0, '', '']
  expect(await folded($, world)).toBe('Not folded: the thread holds nothing yet.')
  expect(world.submits).toHaveLength(1)
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

const CWD = '/repo'
const OFF = ['DISABLE_AUTO_COMPACT', '1']

/**
 * The session coming up where the settings hold `autoCompactEnabled` as `held` (unset while
 * undefined, unreadable while `held.fails`) and the engine answers each `$.env.set` with
 * `answer`'s result.
 */
function coming(
  on: On,
  held?: boolean | { fails: true },
  answer: () => { value: { deny?: string } } | { deny: string } = () => ({ value: {} }),
) {
  const commands: string[] = []
  const sets: (string | undefined)[][] = []
  on('command.register', (_$, e) => {
    commands.push(e.name)
    return { value: { command: e.name } }
  })
  on('session.start', (_$, e) => ({ cwd: e.cwd }))
  on('settings.read', () => {
    // The kit skips a hook that throws, so the read finds no implementation and rejects.
    if (typeof held === 'object') throw new Error('unreadable')
    return { value: held === undefined ? {} : { autoCompactEnabled: held } }
  })
  on('env.set', (_$, e) => {
    sets.push([e.name, e.value])
    return answer()
  })
  return { commands, sets }
}

const come = ($: Engine) => $.session.start({ cwd: CWD, surface: 'terminal', isInteractive: true })

test('a session starts with automatic compaction off', async ($, on) => {
  const world = engine(on, views)
  const unset = coming(on)
  expect(await come($)).toEqual({ cwd: CWD })
  expect(unset.sets).toEqual([OFF])
  expect(unset.commands).toEqual(['fold', 'ai', 'recall', 'reply'])
  expect(world.logs).toEqual([])
})

test('a session whose settings turn automatic compaction off starts with it off', async ($, on) => {
  const world = engine(on, views)
  const off = coming(on, false)
  expect(await come($)).toEqual({ cwd: CWD })
  expect(off.sets).toEqual([OFF])
  expect(world.logs).toEqual([])
})

test('a session whose settings turn automatic compaction on keeps it', async ($, on) => {
  const world = engine(on, views)
  const kept = coming(on, true)
  expect(await come($)).toEqual({ cwd: CWD })
  expect(kept.sets).toEqual([])
  expect(kept.commands).toEqual(['fold', 'ai', 'recall', 'reply'])
  expect(world.logs).toEqual([])
})

test('a session the engine keeps automatic compaction on in says why, and starts as it would', async ($, on) => {
  const world = engine(on, views)
  const held = coming(on, undefined, () => ({ value: { deny: 'your organization manages this variable' } }))
  expect(await come($)).toEqual({ cwd: CWD })
  expect(held.sets).toEqual([OFF])
  expect(held.commands).toEqual(['fold', 'ai', 'recall', 'reply'])
  expect(world.logs).toEqual(['Automatic compaction stays on: your organization manages this variable'])
})

test('a refused switch for automatic compaction says why, and the session starts as it would', async ($, on) => {
  const world = engine(on, views)
  const refused = coming(on, undefined, () => ({ deny: 'not from a plugin' }))
  expect(await come($)).toEqual({ cwd: CWD })
  expect(refused.commands).toEqual(['fold', 'ai', 'recall', 'reply'])
  expect(world.logs.length).toBe(1)
  expect(world.logs[0]).toMatch(/^Automatic compaction stays on: .*not from a plugin/)
})

test('a session whose settings cannot be read keeps automatic compaction on, says why, and starts as it would', async ($, on) => {
  const world = engine(on, views)
  const unread = coming(on, { fails: true })
  expect(await come($)).toEqual({ cwd: CWD })
  expect(unread.sets).toEqual([])
  expect(unread.commands).toEqual(['fold', 'ai', 'recall', 'reply'])
  expect(world.logs.length).toBe(1)
  expect(world.logs[0]).toMatch(/^Automatic compaction stays on: ./)
})

test('a subagent compacts natively, leaving the view /fold built for the main conversation', async ($, on) => {
  const world = engine(on, unattached)
  await start($, 'resume')
  expect(await folded($, world)).toBe('Folding.')
  await $.session.compact({ trigger: 'auto', agentId: 'a1', messages: [] })
  expect(world.compacts).toEqual(['/compact', 'auto'])
  expect(await compact($)).toEqual([
    { role: 'user', text: WOKE, toolUses: [] },
    { role: 'assistant', text: HANDOFF, toolUses: [] },
  ])
})

test('a handoff turn that is stopped, refused, fails or writes nothing folds nothing and says why', async ($, on) => {
  const world = engine(on, carried)
  clearing(on, world)
  await start($, 'resume')
  const ends: Ended[] = [
    { reason: 'aborted', answer: '' },
    { reason: 'refusal', answer: '' },
    { reason: 'error', answer: '' },
    { reason: 'answer', answer: ' \n' },
  ]
  const said: (string | undefined)[] = []
  for (const end of ends) said.push(await folded($, world, end))
  expect(said).toEqual([
    'Not folded: the handoff was stopped, so this session is as it was.',
    'Not folded: the model refused to write the handoff, so this session is as it was.',
    'Not folded: the handoff turn failed, so this session is as it was.',
    'Not folded: the handoff turn ended with no answer, so this session is as it was.',
  ])
  expect(world.calls).toEqual(Array(4).fill(['ai', 'agent', 'compact-session', ...ALL_ARGS]))
  expect(world.logs).toEqual([])
  expect(await carriedOn($, world)).toBe('Folding.')
})

test('a refused request for a handoff folds nothing and says why', async ($, on) => {
  const world = engine(on, carried)
  await start($, 'resume')
  world.submitting.drop = 'a Stop stands'
  expect((await ask($, world)).text).toBe(ASKING)
  await world.clock.advance(0)
  expect(world.logs).toEqual([
    'Not folded: the request for a handoff was refused (a Stop stands), so this session is as it was.',
  ])
  world.submitting.drop = undefined
  expect((await ask($, world)).text).toBe(ASKING)
})

test('/fold while a fold runs is under way; a request whose turn never came is asked again', async ($, on) => {
  const world = engine(on, carried)
  clearing(on, world)
  await start($, 'resume')
  expect((await ask($, world)).text).toBe(ASKING)
  await world.clock.advance(0)
  expect((await ask($, world)).text).toBe(ASKING)
  await world.clock.advance(0)
  expect(world.submits).toHaveLength(2)
  await $.turn.start({ text: REQUEST, turnId: HANDOFF_TURN })
  const ended = complete($, WROTE)
  await world.clock.advance(0)
  expect((await fold($)).text).toBe('Not folded: a fold is under way.')
  await world.clock.advance(FLUSH_MS)
  await ended
  expect(world.logs).toEqual(['Folding.'])
})

test('a turn other than the handoff’s folds nothing', async ($, on) => {
  const world = engine(on, carried)
  await start($, 'resume')
  expect((await ask($, world)).text).toBe(ASKING)
  await world.clock.advance(0)
  await $.turn.start({ text: 'Something else.', turnId: 't0' })
  await $.turn.complete({ turnId: 't0', durationMs: 1, isAborted: false, answer: 'Done.', reason: 'answer' })
  await $.turn.start({ text: REQUEST, turnId: 'a1', agentId: 'a1' })
  await $.turn.complete({ turnId: 'a1', agentId: 'a1', durationMs: 1, isAborted: false, answer: 'Done.', reason: 'answer' })
  await world.clock.advance(FLUSH_MS)
  expect(world.calls).toEqual([['ai', 'agent', 'compact-session', ...ALL_ARGS]])
  expect(world.logs).toEqual([])
})

const DIRECTION = 'carry on with  the fix pass, $& and all'

test('/fold with a direction asks for a handoff aimed at it, then starts the new session on it as your words', async ($, on) => {
  const world = engine(on, carried)
  const cleared = clearing(on, world)
  await start($, 'resume')
  expect(await carriedOn($, world, ` ${DIRECTION} `)).toBe('Folding.')
  expect(world.submits).toEqual([
    { text: `${REQUEST} Next: ${DIRECTION}`, context: undefined, origin: OWN },
    { text: DIRECTION, context: undefined, origin: OWN },
  ])
  expect(cleared.rows).toEqual([WOKE])
  expect(world.logs).toEqual(["Folded into a new session. Attached to dev. It holds your memory's view and the thread's."])
})

test('a direction the new session cannot start on is left in the prompt box, and says why', async ($, on) => {
  let attach: Answer = [0, 'Attached to dev.\n', '']
  const world = engine(on, args => (sub(args) === 'carry-session' ? attach : views(args)))
  const cleared = clearing(on, world)
  const fills = box(on)
  await start($, 'resume')

  cleared.append.deny = 'no notes here'
  expect(await carriedOn($, world, DIRECTION)).toBe('Folding.')
  expect(world.logs.splice(0)[1]).toBe('Not started on your direction: the new session holds no view. Your direction is in the prompt box.')

  cleared.append.deny = undefined
  cleared.clear.starts = 's3'
  attach = [1, '', 'Cannot lock the agents.\n']
  expect(await carriedOn($, world, DIRECTION)).toBe('Folding.')
  expect(world.logs.splice(0)[1]).toBe('Not started on your direction: it is attached to no agent. Your direction is in the prompt box.')

  cleared.clear.starts = 's4'
  attach = [0, 'Attached to dev.\n', '']
  expect(await folded($, world, WROTE, DIRECTION)).toBe('Folding.')
  world.submitting.drop = 'a Stop stands'
  await world.clock.advance(FLUSH_MS)
  expect(world.logs.splice(0)[1]).toBe('Not started on your direction: a Stop stands. Your direction is in the prompt box.')
  expect(fills).toEqual(Array(3).fill({ text: DIRECTION, mode: 'replace' }))
  expect(world.submits.map(s => s.text)).toEqual([
    `${REQUEST} Next: ${DIRECTION}`,
    `${REQUEST} Next: ${DIRECTION}`,
    `${REQUEST} Next: ${DIRECTION}`,
    DIRECTION,
  ])
})

test('a direction left where no prompt box takes it is named in the log', async ($, on) => {
  const world = engine(on, carried)
  const cleared = clearing(on, world)
  box(on, () => ({ isFilled: false, refusal: 'no_composer' }))
  await start($, 'resume')
  cleared.append.deny = 'no notes here'
  expect(await carriedOn($, world, DIRECTION)).toBe('Folding.')
  expect(world.logs.at(-1)).toBe(`Not started on your direction: the new session holds no view. The prompt box would not take your direction, which was: ${DIRECTION}`)
})

const BACK = 'Your direction is in the prompt box.'

test('a /fold whose first build fails hands its direction back; a bare one fills nothing', async ($, on) => {
  const world = engine(on, () => [1, '', 'Cannot commit ~/ai/projects/repo: index.lock exists\n'])
  const fills = box(on)
  await start($, 'resume')
  expect((await ask($, world, ` ${DIRECTION} `)).text).toBe(
    `Not folded: Cannot commit ~/ai/projects/repo: index.lock exists. ${BACK}`,
  )
  expect((await ask($, world)).text).toBe('Not folded: Cannot commit ~/ai/projects/repo: index.lock exists')
  expect(fills).toEqual([{ text: DIRECTION, mode: 'replace' }])
  expect(world.submits).toEqual([])
})

test('a direction handed back goes after what you typed since, and is written out whole where no box takes it', async ($, on) => {
  const world = engine(on, () => [1, '', 'Failed.\n'])
  const filling = { takes: true }
  const fills = box(on, () => (filling.takes ? { isFilled: true } : { isFilled: false, refusal: 'dialog' }), {
    text: 'my own draft',
  })
  await start($, 'resume')
  expect((await ask($, world, DIRECTION)).text).toBe(`Not folded: Failed. ${BACK}`)
  expect(fills).toEqual([{ text: `\n\n${DIRECTION}`, mode: 'append' }])
  filling.takes = false
  expect((await ask($, world, DIRECTION)).text).toBe(
    `Not folded: Failed. The prompt box would not take your direction, which was: ${DIRECTION}`,
  )
})

test('cancelling /fold with a direction hands it back', { plugins: [CANCELLING] }, async ($, on) => {
  const world = engine(on, views)
  const fills = box(on)
  world.spawning.hangs = true
  await start($, 'resume')
  const ran = fold($, DIRECTION)
  await world.clock.advance(FLUSH_MS + 5_000)
  expect((await ran).text).toBe('Cancelled.')
  await world.clock.advance(0)
  expect(world.logs).toEqual([`beneath: Not folded: cancelled. ${BACK}`])
  expect(fills).toEqual([{ text: DIRECTION, mode: 'replace' }])
})

test('/fold with a direction while a fold runs, or when it cannot read its instructions, hands the direction back', async ($, on) => {
  const world = engine(on, carried)
  clearing(on, world)
  const fills = box(on)
  await start($, 'resume')
  expect((await ask($, world)).text).toBe(ASKING)
  await world.clock.advance(0)
  await $.turn.start({ text: REQUEST, turnId: HANDOFF_TURN })
  const ended = complete($, WROTE)
  await world.clock.advance(0)
  expect((await fold($, DIRECTION)).text).toBe(`Not folded: a fold is under way. ${BACK}`)
  await world.clock.advance(FLUSH_MS)
  await ended
  await world.clock.advance(FLUSH_MS)
  expect(world.logs).toEqual([
    'Folding.',
    "Folded into a new session. Attached to dev. It holds your memory's view and the thread's.",
  ])

  world.reading.fails = true
  expect((await ask($, world, DIRECTION)).text).toMatch(/^Not folded: .+\. Your direction is in the prompt box\.$/)
  expect(fills).toEqual(Array(2).fill({ text: DIRECTION, mode: 'replace' }))
  expect(world.submits.map(s => s.text)).toEqual([REQUEST])
})

test('a refused request for a handoff with a direction hands the direction back', async ($, on) => {
  const world = engine(on, carried)
  const fills = box(on)
  await start($, 'resume')
  world.submitting.drop = 'a Stop stands'
  expect((await ask($, world, DIRECTION)).text).toBe(ASKING)
  await world.clock.advance(0)
  expect(world.logs).toEqual([
    `Not folded: the request for a handoff was refused (a Stop stands), so this session is as it was. ${BACK}`,
  ])
  expect(fills).toEqual([{ text: DIRECTION, mode: 'replace' }])
})

test('a handoff turn with a direction that writes no handoff hands the direction back', async ($, on) => {
  const world = engine(on, carried)
  clearing(on, world)
  const fills = box(on)
  await start($, 'resume')
  const ends: Ended[] = [
    { reason: 'aborted', answer: '' },
    { reason: 'refusal', answer: '' },
    { reason: 'error', answer: '' },
    { reason: 'answer', answer: ' \n' },
  ]
  const said: (string | undefined)[] = []
  for (const end of ends) said.push(await folded($, world, end, DIRECTION))
  expect(said).toEqual([
    `Not folded: the handoff was stopped, so this session is as it was. ${BACK}`,
    `Not folded: the model refused to write the handoff, so this session is as it was. ${BACK}`,
    `Not folded: the handoff turn failed, so this session is as it was. ${BACK}`,
    `Not folded: the handoff turn ended with no answer, so this session is as it was. ${BACK}`,
  ])
  expect(fills).toEqual(Array(4).fill({ text: DIRECTION, mode: 'replace' }))
})

test('a fold with a direction that fails after its handoff hands the direction back: the view, the clear', async ($, on) => {
  let view: Answer = [1, '', 'memory locked\n']
  const world = engine(on, args => (sub(args) === 'view' ? view : carried(args)))
  const cleared = clearing(on, world)
  const fills = box(on)
  await start($, 'resume')
  expect(await folded($, world, WROTE, DIRECTION)).toBe(`Not folded: memory locked. ${BACK}`)

  view = [0, WOKE, '']
  cleared.clear.fails = true
  expect(await carriedOn($, world, DIRECTION)).toBe('Folding.')
  expect(world.logs.pop()).toMatch(/^Not folded: \/clear failed, so this session is as it was\. .+ Your direction is in the prompt box\.$/)
  cleared.clear.fails = false
  cleared.clear.starts = SESSION
  expect(await carriedOn($, world, DIRECTION)).toBe('Folding.')
  expect(world.logs.pop()).toBe(`Not folded: /clear started no new session, so this session is as it was. ${BACK}`)
  expect(fills).toEqual(Array(3).fill({ text: DIRECTION, mode: 'replace' }))
  expect(world.submits.map(s => s.text)).toEqual(Array(3).fill(`${REQUEST} Next: ${DIRECTION}`))
  expect(world.logs).toEqual([])
})

test('a /compact that fails in an unattached session hands the direction back', async ($, on) => {
  const world = engine(on, unattached)
  const fills = box(on)
  await start($, 'resume')
  world.compacting.fails = true
  expect(await folded($, world, WROTE, DIRECTION)).toBe('Folding.')
  await world.clock.advance(0)
  expect(world.logs).toHaveLength(1)
  expect(world.logs[0]).toMatch(/^Not folded: \/compact failed\. .+ Your direction is in the prompt box\.$/)
  expect(fills).toEqual([{ text: DIRECTION, mode: 'replace' }])
  expect(world.submits.map(s => s.text)).toEqual([`${REQUEST} Next: ${DIRECTION}`])
})

test('/fold with a direction in an unattached session compacts it, then starts on the direction', async ($, on) => {
  const world = engine(on, unattached)
  await start($, 'resume')
  expect(await folded($, world, WROTE, DIRECTION)).toBe('Folding.')
  expect(world.compacts).toEqual(['/compact'])
  expect(world.submits.map(s => s.text)).toEqual([`${REQUEST} Next: ${DIRECTION}`, DIRECTION])
})

test('/ai attach and detach run ai agent with the session and show its answer', async ($, on) => {
  const world = engine(on, args =>
    sub(args) === 'detach' ? [1, '', 'This session is not attached to an agent.\n'] : attaching(args),
  )
  await start($)
  expect((await ai($, 'attach dev')).text).toBe('Attached to dev. Renaming this session repo.dev.')
  expect((await ai($, ' detach ')).text).toBe('This session is not attached to an agent.')
  expect((await ai($, 'attach')).text).toMatch(/^Usage/)
  expect((await ai($, 'fold')).text).toMatch(/^Usage/)
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'attach', 'dev', ...ALL_ARGS],
    ['ai', 'agent', 'session-name', 'dev'],
    ['ai', 'agent', 'detach', ...ALL_ARGS],
  ])
  await world.clock.advance(0)
  expect(world.renames).toEqual(['repo.dev'])
})

test('/ai attach names the session by the agent’s address once its own run has returned', async ($, on) => {
  const world = engine(on, attaching)
  await start($)
  expect((await ai($, 'attach dev')).text).toBe('Attached to dev. Renaming this session repo.dev.')
  expect(world.renames).toEqual([])
  await world.clock.advance(0)
  expect(world.renames).toEqual(['repo.dev'])
  expect(world.logs).toEqual([])
})

test('/ai detach detaches, then has Claude Code name the session from its conversation', async ($, on) => {
  const world = engine(on, attaching)
  await start($)
  expect((await ai($, 'detach')).text).toBe('Detached from dev. Renaming this session from its conversation.')
  await world.clock.advance(0)
  expect(world.renames).toEqual([''])
})

test('/ai attach renames nothing when attaching or naming fails, and says why', async ($, on) => {
  const world = engine(on, args =>
    args[2] === 'taken'
      ? [1, '', 'No agent taken in this project. Create it: ai agent new taken\n']
      : sub(args) === 'session-name'
        ? [1, '', 'Claude Code cuts a session’s name at 200 characters, and repo.dev has 201.\n']
        : attaching(args),
  )
  await start($)
  expect((await ai($, 'attach taken')).text).toBe('No agent taken in this project. Create it: ai agent new taken')
  expect((await ai($, 'attach dev')).text).toBe(
    'Attached to dev. Not renamed: Claude Code cuts a session’s name at 200 characters, and repo.dev has 201.',
  )
  await world.clock.advance(0)
  expect(world.renames).toEqual([])
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'attach', 'taken', ...ALL_ARGS],
    ['ai', 'agent', 'attach', 'dev', ...ALL_ARGS],
    ['ai', 'agent', 'session-name', 'dev'],
  ])
})

test('a /rename that fails after /ai attach is logged', async ($, on) => {
  const world = engine(on, attaching)
  world.renaming.fails = true
  await start($)
  await ai($, 'attach dev')
  await world.clock.advance(0)
  expect(world.renames).toEqual([])
  expect(world.logs).toEqual([expect.stringMatching(/^ai: \/rename repo\.dev failed: /)])
})

test('/ai alone prints its usage and runs nothing', async ($, on) => {
  const world = engine(on, views)
  await start($, 'resume')
  expect((await ai($, '')).text).toBe('Usage: /ai new <name> | /ai attach <name> | /ai detach')
  expect((await ai($, '  ')).text).toMatch(/^Usage: \/ai new/)
  expect(world.calls).toEqual([])
})

test('/reply is a command that runs mid-turn', async ($, on) => {
  const specs: CommandSpec[] = []
  on('command.register', async (_$, e) => {
    specs.push(e)
    return { value: { command: e.name } }
  })
  on('session.start', async (_$, e) => ({ cwd: e.cwd }))
  await $.session.start({ cwd: '/repo', surface: 'terminal', isInteractive: true })
  const spec = specs.find(s => s.name === 'reply')
  expect(spec?.immediate).toBe(true)
})

test('/reply puts the answer ai reply quotes into the prompt box at the cursor, then a line to answer on', async ($, on) => {
  const QUOTED = '> The docs fork is done.\n>\n> It added a section.\n\n> The tests fork is done too.\n'
  const world = engine(on, () => [0, QUOTED, ''])
  const fills = box(on)
  selecting(on)
  await start($, 'resume')
  expect((await reply($)).text).toBeUndefined()
  expect((await reply($, ' -2#1 ')).text).toBeUndefined()
  expect((await reply($, '--help')).text).toBeUndefined()
  expect((await reply($, '595  -1\t597#2 ')).text).toBeUndefined()
  expect((await reply($, ' "595, -1" ')).text).toBeUndefined()
  // ai reply splits the references and drops their quotes, so the plugin passes them on as typed.
  expect(world.calls).toEqual([
    ['ai', 'reply', ...ALL_ARGS, '--'],
    ['ai', 'reply', ...ALL_ARGS, '--', '-2#1'],
    ['ai', 'reply', ...ALL_ARGS, '--', '--help'],
    ['ai', 'reply', ...ALL_ARGS, '--', '595  -1\t597#2'],
    ['ai', 'reply', ...ALL_ARGS, '--', '"595, -1"'],
  ])
  expect(fills).toEqual(Array(5).fill({ text: `${QUOTED.trimEnd()}\n\n`, mode: 'insert' }))
})

test('/reply in a session whose start never reached the plugin finds the transcript by its id', async ($, on) => {
  const world = engine(on, () => [0, '> Done.\n', ''])
  box(on)
  selecting(on)
  expect((await reply($)).text).toBeUndefined()
  expect(world.calls).toEqual([['ai', 'reply', ...SESSION_ARGS, '--']])
})

test('/reply with nothing after it quotes the text selected with the mouse, else the latest answer', async ($, on) => {
  const world = engine(on, () => [0, '> Latest.\n', ''])
  const fills = box(on)
  const selection = selecting(on)
  await start($, 'resume')
  selection.text = '\n\n  - Should I fix it?  \r\n\n  - And the docs?\n\n'
  expect((await reply($)).text).toBeUndefined()
  expect((await reply($, '   ')).text).toBeUndefined()
  selection.text = ' \n\t'
  expect((await reply($)).text).toBeUndefined()
  selection.text = undefined
  expect((await reply($)).text).toBeUndefined()
  expect(world.calls).toEqual([
    ['ai', 'reply', ...ALL_ARGS, '--'],
    ['ai', 'reply', ...ALL_ARGS, '--'],
  ])
  const selected = { text: '>   - Should I fix it?\n>\n>   - And the docs?\n\n', mode: 'insert' }
  const latest = { text: '> Latest.\n\n', mode: 'insert' }
  expect(fills).toEqual([selected, selected, latest, latest])
})

test('/reply with references quotes what they pick, whatever is selected', async ($, on) => {
  const world = engine(on, () => [0, '> Older.\n', ''])
  const fills = box(on)
  const selection = selecting(on)
  await start($, 'resume')
  selection.text = 'Selected.'
  expect((await reply($, '-2')).text).toBeUndefined()
  expect(world.calls).toEqual([['ai', 'reply', ...ALL_ARGS, '--', '-2']])
  expect(fills).toEqual([{ text: '> Older.\n\n', mode: 'insert' }])
})

test('/reply says why it quotes nothing: ai reply fails, or the prompt box takes nothing', async ($, on) => {
  const PICKS_NONE = 'The latest answer has 2 replies, so #3 picks none. To quote part of an answer, select it and run /reply.'
  let answer: Answer = [1, '', `${PICKS_NONE}\n`]
  engine(on, () => answer)
  const fills = box(on, () => ({ isFilled: false }))
  selecting(on)
  await start($, 'resume')
  expect((await reply($, '#3')).text).toBe(`Not quoted: ${PICKS_NONE}`)
  expect(fills).toEqual([])
  answer = [0, '> Done.\n', '']
  // The kit strips the cause a hook gives, so the engine's own refusals (a dialog, no box) go untested.
  expect((await reply($)).text).toBe('Not quoted: the prompt box did not take it.')
  expect(fills).toEqual([{ text: '> Done.\n\n', mode: 'insert' }])
})

test('/reply quotes the answer and shows what ai reply warned of beside it', async ($, on) => {
  const ALONE =
    "ai reply read this session's transcript alone, as it cannot read this project's agents: This agent store is format 99; this ai reads up to 6. Upgrade ai."
  const world = engine(on, args => (args[0] === 'reply' ? [0, '> Done.\n', `${ALONE}\n`] : [0, '', '']))
  const fills = box(on)
  selecting(on)
  await start($, 'resume')
  expect((await reply($)).text).toBeUndefined()
  expect(fills).toEqual([{ text: '> Done.\n\n', mode: 'insert' }])
  expect(world.logs).toEqual([ALONE])
})

test('/ai new creates the agent, then attaches the session to it; a failed new attaches nothing', async ($, on) => {
  const world = engine(on, args =>
    args[2] === 'taken'
      ? [1, '', 'Agent taken already exists in this project.\n']
      : sub(args) === 'new'
        ? [0, 'Created agent dev. Start it: ai agent resume dev\n', '']
        : attaching(args),
  )
  await start($)
  expect((await ai($, 'new dev')).text).toBe('Created agent dev. Attached to dev. Renaming this session repo.dev.')
  expect((await ai($, 'new taken')).text).toBe('Agent taken already exists in this project.')
  expect((await ai($, 'new')).text).toMatch(/^Usage/)
  expect(world.calls).toEqual([
    ['ai', 'agent', 'compact-session', ...ALL_ARGS],
    ['ai', 'agent', 'new', 'dev'],
    ['ai', 'agent', 'attach', 'dev', ...ALL_ARGS],
    ['ai', 'agent', 'session-name', 'dev'],
    ['ai', 'agent', 'new', 'taken'],
  ])
  await world.clock.advance(0)
  expect(world.renames).toEqual(['repo.dev'])
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
function recalling(on: On, world: World, spawn: Spawn = async () => ({ model: 'm' })) {
  const prompts: { prompt: string }[] = []
  const passed: string[] = []
  on('agent.spawn', async (_$, e) => {
    prompts.push(e)
    return spawn(e.prompt)
  })
  on('tool.call', async (_$, e) => {
    passed.push(e.tool === 'Bash' ? e.command : e.tool)
    return { result: 'ok' }
  })
  return { reads: world.reads, prompts, passed }
}

function begin($: Engine, text: string, agentId: string) {
  return $.turn.start({ text, turnId: `t-${agentId}`, agentId })
}

function call($: Engine, tool: string, input: Record<string, string>, agentId = FORK) {
  return $.tool.call({ tool, ...input, agentId } as Parameters<typeof $.tool.call>[0])
}

test('/recall starts a fork on the message and tells the model how to relay its brief', async ($, on) => {
  const world = engine(on, views)
  const got = recalling(on, world)
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
  const got = recalling(on, engine(on, views))
  expect((await recall($, '')).text).toBe('Usage: /recall <message>')
  expect((await recall($, '   ')).text).toBe('Usage: /recall <message>')
  expect(got.prompts).toEqual([])
})

test('a recall the engine refuses says why, and leaves nothing to guard', async ($, on) => {
  const got = recalling(on, engine(on, views), async () => ({ deny: 'past a Stop' }))
  expect((await recall($, 'the compiler')).text).toBe('Not recalling: past a Stop')
  await begin($, got.prompts[0]!.prompt, FORK)
  expect((await call($, 'Bash', { command: 'ai memory note x' })).deny).toBeUndefined()
})

test('a recall fork reads, and every tool call that could write is refused', async ($, on) => {
  const got = recalling(on, engine(on, views))
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
  const got = recalling(on, engine(on, views), async prompt => {
    await begin($, prompt, FORK)
    return { model: 'm' }
  })
  expect((await recall($, 'the compiler')).text).toBe(RECALLING)
  expect((await call($, 'Bash', { command: 'ai memory note x' })).deny).toMatch(/^A recall only reads/)
  expect(got.passed).toEqual([])
})

test('/fold in an unattached session folds nothing and says why when the plugin never saw the session start', async ($, on) => {
  const world = engine(on, unattached)
  expect(await folded($, world)).toMatch(
    /^Not folded: this session is attached to no agent, so a fold compacts it, and the ai plugin never saw this session start/,
  )
  expect(world.calls).toEqual([['ai', 'agent', 'compact-session', ...SESSION_ARGS]])
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
