import actionIcons from './actionIcons.json'

const builtIn = {
  'files':
    'M14 3H7a2 2 0 0 0-2 2v12M9 7h6l4 4v8a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2V9a2 2 0 0 1 2-2ZM15 7v4h4',
  'search': 'M10.5 17a6.5 6.5 0 1 0 0-13 6.5 6.5 0 0 0 0 13ZM15.5 15.5 20 20',
  'extensions': 'M4 4h6v6H4ZM14 4h6v6h-6ZM4 14h6v6H4ZM17 14v6M14 17h6',
  'problems': 'M12 4 3 20h18L12 4ZM12 10v4M12 17h.01',
  'output': 'M4 5h16v14H4ZM7 9h10M7 12h7M7 15h9',
  // Icons extension views can use (see VIEW_ICONS in the extension host).
  'list': 'M9 6h11M9 12h11M9 18h11M4.5 6h.01M4.5 12h.01M4.5 18h.01',
  'check': 'M5 12.5l4.5 4.5L19 7',
  'bolt': 'M13 3 5 14h6l-1 7 8-11h-6l1-7Z',
  'book': 'M5 19V5a2 2 0 0 1 2-2h12v14H7a2 2 0 0 0-2 2Zm0 0a2 2 0 0 0 2 2h12',
  'bug':
    'M8 9a4 4 0 0 1 8 0v5a4 4 0 0 1-8 0V9ZM12 11v7M4 13h4M16 13h4M5 8l3 2M19 8l-3 2M5 19l3-2M19 19l-3-2',
  'tag': 'M3 12V4h8l10 10-8 8L3 12ZM7.5 8.5h.01',
  'star': 'm12 3.5 2.6 5.3 5.9.9-4.3 4.1 1 5.8-5.2-2.7-5.2 2.7 1-5.8-4.3-4.1 5.9-.9L12 3.5Z',
  'flag': 'M5 21V4h11l-2 4 2 4H5',
  'bell': 'M6 16v-5a6 6 0 0 1 12 0v5l2 2H4l2-2ZM10 20a2 2 0 0 0 4 0',
  'source-control':
    'M6 3v12M6 21a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM18 9a3 3 0 1 0 0-6 3 3 0 0 0 0 6ZM18 9a9 9 0 0 1-9 9',
  'history': 'M3 12a9 9 0 1 0 3-6.7L3 8M3 3v5h5M12 7v5l4 2',
  'cube': 'M12 3l8 4.5v9L12 21l-8-4.5v-9L12 3ZM4 7.5l8 4.5 8-4.5M12 12v9',
} as const

/** Named icons extensions use for tab actions (`scripts/action-icons.mjs`). */
export type ActionIconName = keyof typeof actionIcons

const paths: Record<keyof typeof builtIn | ActionIconName, string> = { ...builtIn, ...actionIcons }

export type IconName = keyof typeof paths

export const isActionIcon = (name: string): name is ActionIconName =>
  Object.hasOwn(actionIcons, name)

interface IconProps {
  name: IconName
  size?: number
}

/** Stroke icon drawn in the current text color. Decorative; label the control instead. */
export function Icon({ name, size = 24 }: IconProps) {
  return (
    <svg
      width={size}
      height={size}
      viewBox="0 0 24 24"
      fill="none"
      stroke="currentColor"
      strokeWidth={1.5}
      strokeLinecap="round"
      strokeLinejoin="round"
      aria-hidden="true"
    >
      <path d={paths[name]} />
    </svg>
  )
}
