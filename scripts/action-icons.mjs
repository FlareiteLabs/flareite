#!/usr/bin/env node
/**
 * Generates src/components/actionIcons.json: the named icons extensions can
 * use for tab actions and in webview pages (the kit's `flareite-icon`).
 *
 *   node scripts/action-icons.mjs
 *
 * Each is a lucide icon (lucide-react is a dependency; ISC license) turned
 * into one SVG path, as the Icon component draws them (24 x 24, stroked),
 * except those drawn here. Add a name below and run it again.
 */
import { readFileSync, writeFileSync } from 'node:fs'

const ROOT = new URL('..', import.meta.url)
const LUCIDE = new URL('node_modules/lucide-react/dist/esm/icons/', ROOT)
const icons = {
  'zoom-in': 'zoom-in',
  'zoom-out': 'zoom-out',
  'fit': 'maximize',
  'refresh': 'refresh-cw',
  'rotate': 'rotate-cw',
  'flip-horizontal': 'flip-horizontal-2',
  'flip-vertical': 'flip-vertical-2',
  'crop': 'crop',
  'move': 'move',
  'image': 'image',
  'palette': 'palette',
  'grid': 'grid-3x3',
  'layers': 'layers',
  'eye': 'eye',
  'eye-off': 'eye-off',
  'sun': 'sun',
  'moon': 'moon',
  'play': 'play',
  'pause': 'pause',
  'stop': 'square',
  'search': 'search',
  'filter': 'funnel',
  'settings': 'settings',
  'more': 'ellipsis',
  'add': 'plus',
  'remove': 'minus',
  'close': 'x',
  'edit': 'pencil',
  'copy': 'copy',
  'save': 'save',
  'download': 'download',
  'upload': 'upload',
  'open': 'external-link',
  'link': 'link',
  'file': 'file',
  'code': 'code',
  'split': 'columns-2',
  'info': 'info',
  'warning': 'triangle-alert',
  'error': 'circle-x',
  'chevron-left': 'chevron-left',
  'chevron-right': 'chevron-right',
  'chevron-up': 'chevron-up',
  'chevron-down': 'chevron-down',
  'check': 'check',
  'undo': 'undo-2',
  'list-tree': 'list-tree',
  'list-flat': 'list',
  'branch': 'git-branch',
  'commit': 'git-commit-horizontal',
  'merge': 'git-merge',
  'arrow-up': 'arrow-up',
  'arrow-down': 'arrow-down',
  'pilcrow': 'pilcrow',
  'fold': 'fold-vertical',
  'unfold': 'unfold-vertical',
  'stage': 'square-plus',
  'file-text': 'file-text',
  'preview': 'scan-eye',
}

const num = value => Number(value)
const fmt = n => String(Math.round(n * 1000) / 1000)

function toPath([tag, a]) {
  switch (tag) {
    case 'path':
      // A path's first moveto is absolute even when written `m` (the pairs
      // after it being relative lines); joined after another path it wouldn't
      // be, so it's written `M x y` and `l` for the pairs after it.
      return a.d.replace(
        /^\s*m\s*(-?[\d.]+)[\s,]*(-?[\d.]+)(\s*[-\d.])?/,
        (_, x, y, next) => `M${x} ${y}${next ? `l${next.trim()}` : ''}`,
      )
    case 'circle': {
      const [cx, cy, r] = [num(a.cx), num(a.cy), num(a.r)]
      return `M${fmt(cx - r)} ${fmt(cy)}a${fmt(r)} ${fmt(r)} 0 1 0 ${fmt(2 * r)} 0a${fmt(r)} ${fmt(r)} 0 1 0 ${fmt(-2 * r)} 0`
    }
    case 'ellipse': {
      const [cx, cy, rx, ry] = [num(a.cx), num(a.cy), num(a.rx), num(a.ry)]
      return `M${fmt(cx - rx)} ${fmt(cy)}a${fmt(rx)} ${fmt(ry)} 0 1 0 ${fmt(2 * rx)} 0a${fmt(rx)} ${fmt(ry)} 0 1 0 ${fmt(-2 * rx)} 0`
    }
    case 'line':
      return `M${a.x1} ${a.y1}L${a.x2} ${a.y2}`
    case 'rect': {
      const [x, y, w, h] = [num(a.x ?? 0), num(a.y ?? 0), num(a.width), num(a.height)]
      const r = Math.min(num(a.rx ?? a.ry ?? 0), w / 2, h / 2)
      if (r === 0) return `M${fmt(x)} ${fmt(y)}h${fmt(w)}v${fmt(h)}h${fmt(-w)}Z`
      return (
        `M${fmt(x + r)} ${fmt(y)}h${fmt(w - 2 * r)}a${fmt(r)} ${fmt(r)} 0 0 1 ${fmt(r)} ${fmt(r)}` +
        `v${fmt(h - 2 * r)}a${fmt(r)} ${fmt(r)} 0 0 1 ${fmt(-r)} ${fmt(r)}h${fmt(-(w - 2 * r))}` +
        `a${fmt(r)} ${fmt(r)} 0 0 1 ${fmt(-r)} ${fmt(-r)}v${fmt(-(h - 2 * r))}a${fmt(r)} ${fmt(r)} 0 0 1 ${fmt(r)} ${fmt(-r)}Z`
      )
    }
    case 'polyline':
    case 'polygon': {
      const points = a.points.trim().split(/[\s,]+/)
      let d = ''
      for (let i = 0; i < points.length; i += 2)
        d += `${i === 0 ? 'M' : 'L'}${points[i]} ${points[i + 1]}`
      return tag === 'polygon' ? `${d}Z` : d
    }
    default:
      throw new Error(`unsupported element ${tag}`)
  }
}

const out = {}
for (const [name, file] of Object.entries(icons)) {
  let source = readFileSync(new URL(`${file}.mjs`, LUCIDE), 'utf8')
  // Aliases re-export another icon.
  const alias = source.match(/export \{ default \} from '\.\/(.+?)\.mjs'/)
  if (alias) source = readFileSync(new URL(`${alias[1]}.mjs`, LUCIDE), 'utf8')
  const match = source.match(/const __iconData = (\{[\s\S]*?\n\});/)
  if (!match) throw new Error(`no icon data in ${file}`)
  // A JS object literal: { name, size, node, aliases? }.
  const { node } = Function(`return ${match[1]}`)()
  out[name] = node.map(toPath).join('')
}
// Drawn here: lucide has no 1:1.
out['actual-size'] =
  'M4 4h16a2 2 0 0 1 2 2v12a2 2 0 0 1-2 2H4a2 2 0 0 1-2-2V6a2 2 0 0 1 2-2ZM6.5 10l2-1.5V15.5M12 10h.01M12 14h.01M15.5 10l2-1.5V15.5'
const sorted = Object.fromEntries(Object.entries(out).sort(([a], [b]) => a.localeCompare(b)))
writeFileSync(
  new URL('src/components/actionIcons.json', ROOT),
  JSON.stringify(sorted, null, 2) + '\n',
)
console.log(Object.keys(sorted).length, 'icons')
