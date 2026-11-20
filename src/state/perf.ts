/**
 * Frontend performance instrumentation: named series of measurements (the
 * most recent samples are kept for exact percentiles) and a signal for when
 * an editor has painted, which benchmarks wait on.
 */

const MAX_SAMPLES = 2000

export type Unit = 'ms' | 'count'

class Series {
  readonly samples: number[] = []
  private next = 0
  count = 0
  sum = 0
  max = 0

  constructor(readonly unit: Unit) {}

  add(value: number): void {
    if (this.samples.length < MAX_SAMPLES) {
      this.samples.push(value)
    } else {
      this.samples[this.next] = value
      this.next = (this.next + 1) % MAX_SAMPLES
    }
    this.count += 1
    this.sum += value
    this.max = Math.max(this.max, value)
  }
}

export interface SeriesSummary {
  name: string
  unit: Unit
  count: number
  mean: number
  p50: number
  p95: number
  p99: number
  max: number
}

const series = new Map<string, Series>()

function record(name: string, unit: Unit, value: number): void {
  let entry = series.get(name)
  if (!entry) {
    entry = new Series(unit)
    series.set(name, entry)
  }
  entry.add(value)
}

export const recordTiming = (name: string, ms: number) => record(name, 'ms', ms)
export const recordCount = (name: string, value: number) => record(name, 'count', value)

export function percentile(sorted: readonly number[], fraction: number): number {
  if (sorted.length === 0) {
    return 0
  }
  const index = Math.min(sorted.length - 1, Math.ceil(fraction * sorted.length) - 1)
  return sorted[Math.max(0, index)] ?? 0
}

/** Percentiles over recent samples; counts, means, and maxima over all. */
export function summarize(name: string, unit: Unit, values: readonly number[]): SeriesSummary {
  const sorted = [...values].sort((a, b) => a - b)
  const sum = values.reduce((total, value) => total + value, 0)
  return {
    name,
    unit,
    count: values.length,
    mean: values.length ? sum / values.length : 0,
    p50: percentile(sorted, 0.5),
    p95: percentile(sorted, 0.95),
    p99: percentile(sorted, 0.99),
    max: sorted.at(-1) ?? 0,
  }
}

export function perfSnapshot(): SeriesSummary[] {
  return [...series.entries()]
    .map(([name, entry]) => ({
      ...summarize(name, entry.unit, entry.samples),
      count: entry.count,
      mean: entry.count ? entry.sum / entry.count : 0,
      max: entry.max,
    }))
    .sort((a, b) => a.name.localeCompare(b.name))
}

export function resetPerf(): void {
  series.clear()
}

export interface PaintInfo {
  documentId: number
  version: number
  startLine: number
  endLine: number
}

/** What a minimap last drew, which the fast-scrolling benchmark checks each frame. */
export interface MinimapDraw {
  documentId: number
  /** The editor's scroll position the minimap was drawn for. */
  scrollTop: number
  /** Rows the minimap shows but had no lines for, so left blank. */
  missingRows: number
}

let lastMinimapDraw: MinimapDraw | null = null

export function noteMinimapDraw(draw: MinimapDraw): void {
  lastMinimapDraw = draw
}

export const lastMinimapDrawn = (): MinimapDraw | null => lastMinimapDraw

const paintListeners = new Set<(info: PaintInfo) => void>()

/** Called by the editor view after a view has been rendered and painted. */
export function notifyPainted(info: PaintInfo): void {
  for (const listener of [...paintListeners]) {
    listener(info)
  }
}

/** Resolves on the next paint matching `accept`. Subscribe before acting. */
export function nextPaint(accept: (info: PaintInfo) => boolean = () => true): Promise<PaintInfo> {
  return new Promise(resolve => {
    const listener = (info: PaintInfo) => {
      if (accept(info)) {
        paintListeners.delete(listener)
        resolve(info)
      }
    }
    paintListeners.add(listener)
  })
}
