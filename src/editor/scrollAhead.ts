/**
 * How many rows the minimap keeps beyond those it shows, in a document too
 * large to fetch whole (`minimapRows.ts`).
 *
 * Rows come from the backend, so a scroll that runs past the rows held
 * draws nothing until more arrive. While scrolling, rows are therefore kept
 * ahead, in the direction of scrolling: at least a screen, more the faster
 * it goes. They're asked for again once a quarter of them have scrolled into
 * view, so the rest covers the time the request takes. When scrolling
 * stops, a small margin on each side is enough.
 *
 * (The editor doesn't do this: it draws rows its view lacks from its copy
 * of the lines, `lineStore.ts`. Views carrying hundreds of rows ahead made
 * it render them in the middle of a scroll, which made scrolling lag.)
 */

/** Rows kept on each side when not scrolling (the minimap keeps at least its own overscan). */
export const OVERSCAN = 20
/** Scrolling time to keep rows ahead for: several round trips. */
const LOOKAHEAD_MS = 150
/** Most rows kept ahead. */
export const MAX_AHEAD = 600
/** A pause between scroll events longer than this starts a new scroll. */
const GESTURE_GAP_MS = 150
/**
 * A move of more rows than this at once is a jump (dragging the scrollbar,
 * Go to Line), which keeps nothing ahead: half the most rows ahead, which
 * couldn't keep up with longer moves anyway.
 */
const JUMP_ROWS = MAX_AHEAD / 2
/** How fast the remembered speed decays while scrolling slows. */
const SLOWING = 0.85

export interface Motion {
  top: number
  time: number
  /** Scrolling down (1), up (-1), or neither (0). */
  direction: -1 | 0 | 1
  /** Pixels per millisecond, in the direction of scrolling. */
  speed: number
}

export interface Margins {
  above: number
  below: number
}

export const STILL: Margins = { above: OVERSCAN, below: OVERSCAN }

/** The motion after a scroll to `top` at `time`. */
export function nextMotion(
  previous: Motion | null,
  top: number,
  time: number,
  rowHeight: number,
): Motion {
  const delta = previous ? top - previous.top : 0
  const jump = Math.abs(delta) > JUMP_ROWS * rowHeight
  if (!previous || delta === 0 || jump || time - previous.time > GESTURE_GAP_MS) {
    // A jump keeps nothing ahead; a new scroll knows only its direction.
    const direction = previous && !jump ? Math.sign(delta) : 0
    return { top, time, direction: direction as Motion['direction'], speed: 0 }
  }
  const direction = Math.sign(delta) as Motion['direction']
  const speed = Math.abs(delta) / Math.max(1, time - previous.time)
  return {
    top,
    time,
    direction,
    // Quick to follow a faster scroll, slow to forget it: wheel scrolling
    // arrives in uneven steps.
    speed: direction === previous.direction ? Math.max(speed, previous.speed * SLOWING) : speed,
  }
}

/** The rows to keep above and below the visible ones while moving as `motion`. */
export function marginsFor(motion: Motion | null, lineHeight: number, screenRows: number): Margins {
  if (!motion || motion.direction === 0) {
    return STILL
  }
  const ahead = Math.min(
    MAX_AHEAD,
    Math.max(screenRows, OVERSCAN, Math.ceil((motion.speed / lineHeight) * LOOKAHEAD_MS)),
  )
  return motion.direction > 0
    ? { above: OVERSCAN, below: ahead }
    : { above: ahead, below: OVERSCAN }
}

/**
 * The rows that must already be held beyond the visible ones: three
 * quarters of the margin ahead. Nothing extra is needed behind or when
 * still.
 */
export function neededFor(margins: Margins): Margins {
  return {
    above: margins.above > OVERSCAN ? Math.ceil((margins.above * 3) / 4) : 0,
    below: margins.below > OVERSCAN ? Math.ceil((margins.below * 3) / 4) : 0,
  }
}
