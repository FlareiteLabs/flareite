import { describe, expect, it } from 'vitest'
import {
  MAX_AHEAD,
  OVERSCAN,
  STILL,
  marginsFor,
  neededFor,
  nextMotion,
  type Motion,
} from './scrollAhead'

// 20px lines and a 1,000px (50-row) screen.
const lineHeight = 20
const screenRows = 50

/** Scroll events `step` pixels apart, one per 16 ms frame. */
function scroll(steps: number[], from: Motion | null = null): Motion | null {
  let motion = from
  let top = motion?.top ?? 10_000
  let time = motion?.time ?? 0
  for (const step of steps) {
    top += step
    time += 16
    motion = nextMotion(motion, top, time, lineHeight)
  }
  return motion
}

describe('rows kept ahead while scrolling', () => {
  it('keeps a small margin on both sides when still', () => {
    expect(marginsFor(null, lineHeight, screenRows)).toEqual(STILL)
    expect(neededFor(STILL)).toEqual({ above: 0, below: 0 })
  })

  it('keeps at least a screen ahead in the direction of a slow scroll', () => {
    const down = scroll([0, 20, 20])
    expect(marginsFor(down, lineHeight, screenRows)).toEqual({ above: OVERSCAN, below: 50 })
    const up = scroll([0, -20, -20])
    expect(marginsFor(up, lineHeight, screenRows)).toEqual({ above: 50, below: OVERSCAN })
  })

  it('keeps more ahead the faster it scrolls, up to a limit', () => {
    // 40 lines a frame: 2.5 lines per ms, 375 lines in 150 ms.
    const fast = scroll([800, 800, 800])
    expect(marginsFor(fast, lineHeight, screenRows).below).toBe(375)
    const faster = scroll([2000, 2000, 2000])
    expect(marginsFor(faster, lineHeight, screenRows).below).toBe(MAX_AHEAD)
  })

  it('remembers a fast scroll through uneven steps', () => {
    const fast = scroll([800, 800, 100])
    expect(marginsFor(fast, lineHeight, screenRows).below).toBeGreaterThan(300)
  })

  it('asks again once a quarter of the rows ahead have scrolled into view', () => {
    expect(neededFor({ above: OVERSCAN, below: 400 })).toEqual({ above: 0, below: 300 })
  })

  it('keeps up with a very fast scroll', () => {
    // 100 lines a frame: two screens.
    const fastest = scroll([2000, 2000, 2000])
    expect(marginsFor(fastest, lineHeight, screenRows).below).toBe(MAX_AHEAD)
  })

  it('treats a jump and a pause as a fresh start', () => {
    const fast = scroll([800, 800])
    // 1,000 lines at once: more than rows ahead could cover.
    const jumped = nextMotion(fast, fast!.top + 1000 * lineHeight, fast!.time + 16, lineHeight)
    expect(marginsFor(jumped, lineHeight, screenRows)).toEqual(STILL)
    const resumed = nextMotion(fast, fast!.top + 20, fast!.time + 500, lineHeight)
    expect(resumed.speed).toBe(0)
    expect(marginsFor(resumed, lineHeight, screenRows)).toEqual({ above: OVERSCAN, below: 50 })
  })
})
