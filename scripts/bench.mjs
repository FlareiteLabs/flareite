#!/usr/bin/env node
/**
 * End-to-end benchmarks against the release build.
 *
 *   pnpm tauri build --no-bundle          # build target/release/flareite first
 *   node scripts/bench.mjs startup        # launch time and idle memory (5 runs)
 *   node scripts/bench.mjs typing         # keystroke-to-paint latency
 *                                         (--file F also types into F, e.g.
 *                                         a source file; add --language
 *                                         extensions/test-fixtures/languages/rust
 *                                         to highlight it)
 *   node scripts/bench.mjs scroll         # page-jump scrolling, 200k lines
 *   node scripts/bench.mjs fling          # fast scrolling: blank frames in the
 *                                         editor and minimap (--file F to
 *                                         scroll a highlighted file)
 *   node scripts/bench.mjs drag           # dragging the minimap through 4,000
 *                                         lines (--file F for a file)
 *   node scripts/bench.mjs width          # the horizontal scroll range keeps
 *                                         to the widest line seen
 *   node scripts/bench.mjs open --file F  # open a file until first paint
 *   node scripts/bench.mjs all            # startup, typing, and scroll
 *
 * Options: --runs N and --pause MS between runs (startup), --binary PATH,
 * --extensions (load extensions, which runs skip by default),
 * --language DIR (load only this extension folder, for its languages;
 * repeatable),
 * --json PATH (default
 * target/bench/results.json). The app runs with `--bench <name>`, writes its
 * report to the file in FLAREITE_BENCH_OUT, and stays up briefly so this
 * script can measure the memory of its whole process tree (the app plus its
 * WebView processes).
 */
import { execFileSync, spawn } from 'node:child_process'
import fs from 'node:fs'
import os from 'node:os'
import path from 'node:path'
import { performance } from 'node:perf_hooks'
import { fileURLToPath } from 'node:url'

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..')
const args = process.argv.slice(2)
const option = (name, fallback) => {
  const index = args.indexOf(`--${name}`)
  return index >= 0 && args[index + 1] ? args[index + 1] : fallback
}
const which =
  args.find(arg => !arg.startsWith('--') && !args[args.indexOf(arg) - 1]?.startsWith('--')) ?? 'all'
const binary = path.resolve(
  option(
    'binary',
    path.join(
      root,
      'target',
      'release',
      process.platform === 'win32' ? 'flareite.exe' : 'flareite',
    ),
  ),
)
const jsonOut = path.resolve(option('json', path.join(root, 'target', 'bench', 'results.json')))
const runs = Number(option('runs', '5'))
const pauseMs = Number(option('pause', '3000'))
const file = option('file', null)
/** `--extensions`: load installed and development extensions too. */
const withExtensions = args.includes('--extensions')
/** `--language DIR`: language extensions to load on their own. */
const languages = args.flatMap((arg, index) =>
  arg === '--language' && args[index + 1]
    ? ['--bench-language', path.resolve(args[index + 1])]
    : [],
)

if (!fs.existsSync(binary)) {
  console.error(`No release build at ${binary}. Run: pnpm tauri build --no-bundle`)
  process.exit(1)
}

/** Memory of `pid` and all its descendants. */
function processTreeMemory(pid) {
  let rows
  if (process.platform === 'win32') {
    const json = execFileSync(
      'powershell',
      [
        '-NoProfile',
        '-Command',
        'Get-CimInstance Win32_Process | Select-Object ProcessId,ParentProcessId,Name,WorkingSetSize,PrivatePageCount | ConvertTo-Json -Compress',
      ],
      { encoding: 'utf8', maxBuffer: 64 * 1024 * 1024 },
    )
    rows = JSON.parse(json).map(p => ({
      pid: p.ProcessId,
      ppid: p.ParentProcessId,
      name: p.Name,
      resident: Number(p.WorkingSetSize),
      private: Number(p.PrivatePageCount),
    }))
  } else {
    const text = execFileSync('ps', ['-A', '-o', 'pid=,ppid=,rss=,comm='], { encoding: 'utf8' })
    rows = text
      .trim()
      .split('\n')
      .map(line => line.trim().split(/\s+/))
      .map(([p, pp, rss, ...name]) => ({
        pid: Number(p),
        ppid: Number(pp),
        name: name.join(' '),
        resident: Number(rss) * 1024,
        private: null,
      }))
  }
  const tree = new Set([pid])
  for (let grew = true; grew;) {
    grew = false
    for (const row of rows) {
      if (!tree.has(row.pid) && tree.has(row.ppid)) {
        tree.add(row.pid)
        grew = true
      }
    }
  }
  const members = rows.filter(row => tree.has(row.pid))
  const sum = key => members.reduce((total, row) => total + (row[key] ?? 0), 0)
  return {
    processes: members.length,
    residentBytes: sum('resident'),
    privateBytes: process.platform === 'win32' ? sum('private') : null,
    appResidentBytes: members.find(row => row.pid === pid)?.resident ?? null,
  }
}

/** Launches one benchmark run and waits for its report. */
async function runOnce(name, extra = []) {
  const out = path.join(os.tmpdir(), `flareite-bench-${process.pid}-${Date.now()}.json`)
  const started = performance.now()
  const flags = [...(withExtensions ? ['--bench-extensions'] : []), ...languages]
  const child = spawn(binary, ['--bench', name, ...extra, ...flags], {
    env: { ...process.env, FLAREITE_BENCH_OUT: out },
    stdio: 'ignore',
  })
  const exited = new Promise(resolve => child.on('exit', resolve))
  const deadline = Date.now() + 180_000
  while (!fs.existsSync(out)) {
    if (Date.now() > deadline || child.exitCode !== null) {
      child.kill()
      throw new Error(`${name}: no report (exit code ${child.exitCode})`)
    }
    await new Promise(resolve => setTimeout(resolve, 20))
  }
  const wallMs = performance.now() - started
  await new Promise(resolve => setTimeout(resolve, 100))
  const memory = processTreeMemory(child.pid)
  const report = JSON.parse(fs.readFileSync(out, 'utf8'))
  fs.rmSync(out, { force: true })
  await exited
  if (!report.result?.ok) {
    throw new Error(`${name}: ${report.result?.error ?? 'failed'}`)
  }
  return { wallMs, memory, report }
}

const median = values => {
  const sorted = [...values].sort((a, b) => a - b)
  return sorted[Math.floor(sorted.length / 2)]
}
const mb = bytes => (bytes == null ? 'n/a' : `${(bytes / 1024 / 1024).toFixed(1)} MB`)
const ms = value => (value == null ? 'n/a' : `${value.toFixed(1)} ms`)

async function startup() {
  const results = []
  for (let run = 0; run < runs; run++) {
    if (run > 0) {
      // Let the previous instance's WebView processes finish exiting.
      await new Promise(resolve => setTimeout(resolve, pauseMs))
    }
    results.push(await runOnce('startup'))
  }
  const ready = r => r.report.backend.launchToFrontendReadyMs
  const rest = results.slice(1).length ? results.slice(1) : results
  const summary = {
    firstRun: { launchToReadyMs: ready(results[0]), memory: results[0].memory },
    laterRuns: {
      runs: rest.length,
      launchToReadyMedianMs: median(rest.map(ready)),
      launchToReadyMinMs: Math.min(...rest.map(ready)),
      launchToReadyMaxMs: Math.max(...rest.map(ready)),
      treeResidentMedianBytes: median(rest.map(r => r.memory.residentBytes)),
      treePrivateMedianBytes:
        rest[0].memory.privateBytes == null ? null : median(rest.map(r => r.memory.privateBytes)),
      appResidentMedianBytes: median(rest.map(r => r.memory.appResidentBytes)),
      processes: rest[0].memory.processes,
    },
  }
  console.log('\nStartup')
  console.log(`  first run         ready in ${ms(summary.firstRun.launchToReadyMs)}`)
  console.log(
    `  later runs (${rest.length})    ready in ${ms(summary.laterRuns.launchToReadyMedianMs)} median ` +
      `(${ms(summary.laterRuns.launchToReadyMinMs)}–${ms(summary.laterRuns.launchToReadyMaxMs)})`,
  )
  console.log(
    `  idle memory       ${mb(summary.laterRuns.treeResidentMedianBytes)} resident across ` +
      `${summary.laterRuns.processes} processes (app ${mb(summary.laterRuns.appResidentMedianBytes)}, ` +
      `private ${mb(summary.laterRuns.treePrivateMedianBytes)})`,
  )
  return summary
}

function printLatency(label, stats) {
  console.log(
    `  ${label.padEnd(26)} p50 ${ms(stats.p50)}  p95 ${ms(stats.p95)}  max ${ms(stats.max)}  (n=${stats.count})`,
  )
}

async function typing() {
  const { report, memory } = await runOnce('typing', file ? [path.resolve(file)] : [])
  const result = report.result
  console.log('\nTyping (keystroke → painted)')
  printLatency('small document', result.smallDocumentKeystroke)
  printLatency('200k-line document', result.largeDocumentKeystroke)
  if (result.fileKeystroke) {
    printLatency(path.basename(file), result.fileKeystroke)
  }
  console.log(`  300-character burst        ${ms(result.burst300CharactersMs)} until settled`)
  console.log(`  memory afterwards          ${mb(memory.residentBytes)} resident`)
  return { ...result, memory }
}

async function scroll() {
  const { report, memory } = await runOnce('scroll')
  const result = report.result
  console.log('\nScrolling (200k lines × 80 columns)')
  console.log(`  generate + open           ${ms(result.generateAndOpen200kLinesMs)}`)
  printLatency('page jump → painted', result.pageJumpToPaint)
  console.log(`  jump to end → painted     ${ms(result.jumpToEndMs)}`)
  console.log(`  memory with document      ${mb(memory.residentBytes)} resident`)
  return { ...result, memory }
}

/** One line per scroll run: frames painted with blanks, and frame times (long ones are lag). */
function printScrolls(runs, label) {
  const pct = (part, whole) => `${((100 * part) / Math.max(1, whole)).toFixed(0)}%`
  for (const run of runs) {
    console.log(
      `  ${label(run)}  editor blank in ${pct(run.editorBlankFrames, run.frames)} of frames ` +
        `(${(100 * run.editorBlankShare).toFixed(1)}% of the area)  ` +
        `minimap blank in ${pct(run.minimapBlankFrames, run.frames)}  ` +
        (run.misplacedRows ? `MISPLACED ROWS ${run.misplacedRows}  ` : '') +
        `frames p50 ${ms(run.frameInterval.p50)} p95 ${ms(run.frameInterval.p95)} ` +
        `max ${ms(run.frameInterval.max)}`,
    )
  }
}

async function fling() {
  const { report } = await runOnce('fling', file ? [path.resolve(file)] : [])
  const result = report.result
  console.log(
    `\nFast scrolling (${file ? path.basename(file) : 'generated'}, ${result.lineCount} lines)`,
  )
  printScrolls(result.flings, run => `${String(run.linesPerFrame).padStart(3)} lines/frame`)
  return result
}

async function drag() {
  const { report } = await runOnce('drag', file ? [path.resolve(file)] : [])
  const result = report.result
  console.log(
    `\nDragging the minimap (${file ? path.basename(file) : 'generated'}, ${result.lineCount} lines)`,
  )
  printScrolls(result.drags, run => `top to bottom in ${String(run.frames / 2).padStart(2)} frames`)
  return result
}

async function width() {
  const { report } = await runOnce('width')
  const result = report.result
  console.log('\nHorizontal scroll range (scrollWidth, px)')
  console.log(
    `  at the top ${result.atTop}, at the long line ${result.atLongLine}, past it ${result.pastIt}`,
  )
  console.log(`  after shortening it ${result.afterShortening}, past it again ${result.backPastIt}`)
  console.log(`  ${result.ok ? 'as expected' : 'NOT AS EXPECTED'}`)
  return result
}

async function open() {
  if (!file) {
    throw new Error('open needs --file <path>')
  }
  const { report, memory } = await runOnce('open', [path.resolve(file)])
  const result = report.result
  console.log(`\nOpen ${file} (${mb(fs.statSync(file).size)})`)
  console.log(`  open → loaded             ${ms(result.openToLoadedMs)}`)
  console.log(`  open → painted            ${ms(result.openToPaintMs)}`)
  console.log(`  memory with document      ${mb(memory.residentBytes)} resident`)
  return { ...result, memory }
}

const suites = { startup, typing, scroll, fling, drag, width, open }
const selected = which === 'all' ? ['startup', 'typing', 'scroll'] : [which]
const results = {
  machine: {
    platform: `${process.platform} ${os.release()}`,
    cpu: os.cpus()[0]?.model,
    cores: os.cpus().length,
    memoryBytes: os.totalmem(),
  },
  date: new Date().toISOString(),
}
for (const name of selected) {
  if (!suites[name]) {
    console.error(`Unknown benchmark "${name}". Choose: ${Object.keys(suites).join(', ')}, all`)
    process.exit(1)
  }
  results[name] = await suites[name]()
}
fs.mkdirSync(path.dirname(jsonOut), { recursive: true })
const previous = fs.existsSync(jsonOut) ? JSON.parse(fs.readFileSync(jsonOut, 'utf8')) : {}
fs.writeFileSync(jsonOut, JSON.stringify({ ...previous, ...results }, null, 2))
console.log(`\nWrote ${path.relative(root, jsonOut)}`)
