/**
 * An extension's preview: formatted text the extension sends as a tree of
 * nodes (headings, paragraphs, lists, tables, code, links, pictures), drawn
 * here as React elements. Nothing an extension sends becomes markup, so it
 * can't inject HTML, scripts, or styles. Extensions' READMEs are drawn the
 * same way (`PreviewDocument`).
 *
 * - **Links** to web and mail addresses open with the system; links to
 *   files open them in the editor (at the line a `#L12` anchor names), and
 *   links to headings scroll there. Ctrl+Shift+click asks who opens them.
 *   The backend resolved each one; those it couldn't are shown as text.
 * - **Pictures** in files the backend resolved are loaded through it; web
 *   pictures aren't loaded (so a preview sends no requests), and show their
 *   description instead.
 * - **Code blocks** in a language Flareite knows are highlighted with the
 *   editor's colors.
 */
import { ImageOff } from 'lucide-react'
import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useMemo,
  useRef,
  useState,
  type KeyboardEvent,
  type ReactNode,
} from 'react'
import { LinkedText } from '../components/LinkedText'
import { openLink } from '../state/links'
import {
  getPreviewContent,
  getPreviewImage,
  type LinkTarget,
  type PreviewContent,
  type PreviewInfo,
  type PreviewNode,
} from '../ipc/extensions'
import { segments } from '../editor/segments'
import { reportError } from '../state/notifications'
import { previewVersions } from '../state/previews'
import { useStore } from '../state/store'
import { WebviewFrame } from './WebviewFrame'
import './PreviewPane.css'

/** A preview's nodes with each one's children, and its headings' anchors. */
interface Tree {
  nodes: readonly PreviewNode[]
  children: readonly (readonly number[])[]
  roots: readonly number[]
  /** By heading node index. */
  anchors: ReadonlyMap<number, string>
  highlights: PreviewContent['highlights']
  /** Web addresses and paths in text are links too (`LinkedText`). */
  linkText?: boolean
}

/** Whether a node is inside a link. */
function insideLink(tree: Tree, index: number): boolean {
  for (let parent = tree.nodes[index]!.parent; parent !== null;) {
    const node = tree.nodes[parent]!
    if (node.type === 'link') {
      return true
    }
    parent = node.parent
  }
  return false
}

/** The line a file link's anchor names (`L12`), from 0. */
function anchorLine(anchor: string | null): number | null {
  const line = anchor && /^L(\d+)/.exec(anchor)?.[1]
  return line ? Math.max(0, Number(line) - 1) : null
}

/** All the text inside a node. */
function textOf(tree: Omit<Tree, 'anchors'>, index: number): string {
  const node = tree.nodes[index]!
  switch (node.type) {
    case 'text':
    case 'code':
      return node.text
    case 'image':
      return node.description
    default:
      return tree.children[index]!.map(child => textOf(tree, child)).join('')
  }
}

/**
 * A heading's anchor as GitHub makes it: lowercase, punctuation removed,
 * spaces as hyphens.
 */
export function slug(text: string): string {
  return text
    .trim()
    .toLowerCase()
    .replace(/[^\p{L}\p{N}\s_-]/gu, '')
    .replace(/\s/g, '-')
}

export function buildTree(content: PreviewContent): Tree {
  const { nodes } = content
  const children: number[][] = nodes.map(() => [])
  const roots: number[] = []
  nodes.forEach((node, index) => {
    if (node.parent === null) {
      roots.push(index)
    } else {
      children[node.parent]?.push(index)
    }
  })
  const partial = { nodes, children, roots, highlights: content.highlights }
  // Repeated anchors get -1, -2, … as on GitHub.
  const anchors = new Map<number, string>()
  const used = new Map<string, number>()
  nodes.forEach((node, index) => {
    if (node.type === 'heading') {
      const base = slug(textOf(partial, index))
      const count = used.get(base) ?? 0
      used.set(base, count + 1)
      anchors.set(index, count === 0 ? base : `${base}-${count}`)
    }
  })
  return { ...partial, anchors }
}

/** What links and pictures need while drawing. */
interface Context {
  /** Loads a picture file as a `data:` URL. */
  loadImage: (path: string) => Promise<string>
  /** Follows a link; `choose` asks who opens it. */
  follow: (target: LinkTarget, choose?: boolean) => void
  /** Data URLs of pictures already loaded, by path. */
  images: Map<string, string>
}

const PreviewContext = createContext<Context | null>(null)

function Link({
  target,
  title,
  children,
}: {
  target: LinkTarget
  title: string | null
  children: ReactNode
}) {
  const context = useContext(PreviewContext)!
  if (target.kind === 'unsupported') {
    return (
      <span className="preview__unsupported-link" title={title ?? target.destination}>
        {children}
      </span>
    )
  }
  const where =
    target.kind === 'web' ? target.url : target.kind === 'file' ? target.path : `#${target.anchor}`
  const onKeyDown = (event: KeyboardEvent) => {
    if (event.key === 'Enter') {
      event.preventDefault()
      context.follow(target)
    }
  }
  // No `href`: the webview must never navigate to what an extension sent.
  return (
    <a
      className="preview__link"
      role="link"
      tabIndex={0}
      title={title ? `${title}\n${where}` : where}
      onClick={event => {
        event.preventDefault()
        context.follow(target, (event.ctrlKey || event.metaKey) && event.shiftKey)
      }}
      onKeyDown={onKeyDown}
    >
      {children}
    </a>
  )
}

function Picture({
  path,
  description,
  title,
}: {
  path: string
  description: string
  title: string | null
}) {
  const context = useContext(PreviewContext)!
  const [source, setSource] = useState(() => context.images.get(path) ?? null)
  const [failed, setFailed] = useState(false)

  useEffect(() => {
    if (context.images.has(path)) {
      setSource(context.images.get(path)!)
      return
    }
    let cancelled = false
    context
      .loadImage(path)
      .then(url => {
        context.images.set(path, url)
        if (!cancelled) {
          setSource(url)
        }
      })
      .catch(() => {
        if (!cancelled) {
          setFailed(true)
        }
      })
    return () => {
      cancelled = true
    }
  }, [context, path])

  if (failed) {
    return <Placeholder description={description} title={title ?? path} />
  }
  return source ? (
    <img className="preview__image" src={source} alt={description} title={title ?? undefined} />
  ) : (
    <span className="preview__image-loading" role="img" aria-label={description} />
  )
}

function Placeholder({ description, title }: { description: string; title: string }) {
  return (
    <span className="preview__image-placeholder" title={title}>
      <ImageOff size={14} strokeWidth={1.5} aria-hidden="true" />
      {description || 'Picture'}
    </span>
  )
}

function CodeBlock({ text, lines }: { text: string; lines: number[][] | undefined }) {
  const rows = text.split('\n')
  return (
    <pre className="preview__code-block">
      <code>
        {rows.map((row, index) => (
          <span key={index} className="preview__code-line">
            {lines?.[index]
              ? segments(row, lines[index]).map(segment =>
                  segment.className ? (
                    <span key={segment.start} className={segment.className}>
                      {row.slice(segment.start, segment.end)}
                    </span>
                  ) : (
                    row.slice(segment.start, segment.end)
                  ),
                )
              : row}
            {index < rows.length - 1 ? '\n' : null}
          </span>
        ))}
      </code>
    </pre>
  )
}

function renderChildren(tree: Tree, index: number): ReactNode[] {
  return tree.children[index]!.map(child => renderNode(tree, child))
}

/** A table: its header row, then its other rows in a body. */
function renderTable(tree: Tree, index: number, alignments: readonly string[]): ReactNode {
  const align = (column: number) => {
    const alignment = alignments[column]
    return alignment && alignment !== 'none' ? { textAlign: alignment as 'left' } : undefined
  }
  const row = (rowIndex: number, Cell: 'th' | 'td') => (
    <tr key={rowIndex}>
      {tree.children[rowIndex]!.map((cell, column) => (
        <Cell key={cell} style={align(column)}>
          {renderChildren(tree, cell)}
        </Cell>
      ))}
    </tr>
  )
  const rows = tree.children[index]!
  const head = rows.filter(child => tree.nodes[child]!.type === 'tableHead')
  const body = rows.filter(child => tree.nodes[child]!.type === 'tableRow')
  return (
    <div key={index} className="preview__table-scroll">
      <table>
        {head.length > 0 && <thead>{head.map(child => row(child, 'th'))}</thead>}
        {body.length > 0 && <tbody>{body.map(child => row(child, 'td'))}</tbody>}
      </table>
    </div>
  )
}

function renderNode(tree: Tree, index: number): ReactNode {
  const node = tree.nodes[index]!
  const children = () => renderChildren(tree, index)
  switch (node.type) {
    case 'paragraph':
      return <p key={index}>{children()}</p>
    case 'heading': {
      const Heading = `h${Math.min(Math.max(node.level, 1), 6)}` as 'h1'
      return (
        <Heading key={index} data-anchor={tree.anchors.get(index)}>
          {children()}
        </Heading>
      )
    }
    case 'quote':
      return <blockquote key={index}>{children()}</blockquote>
    case 'bulletedList':
      return <ul key={index}>{children()}</ul>
    case 'numberedList':
      return (
        <ol key={index} start={node.start}>
          {children()}
        </ol>
      )
    case 'listItem':
      return node.checked === null ? (
        <li key={index}>{children()}</li>
      ) : (
        <li key={index} className="preview__task">
          <input type="checkbox" checked={node.checked} disabled aria-label="Done" />
          {children()}
        </li>
      )
    case 'codeBlock':
      return <CodeBlock key={index} text={node.text} lines={tree.highlights[index]} />
    case 'rule':
      return <hr key={index} />
    case 'table':
      return renderTable(tree, index, node.alignments)
    case 'tableHead':
    case 'tableRow':
    case 'tableCell':
      // Drawn by their table.
      return null
    case 'text':
      return tree.linkText && !insideLink(tree, index) ? (
        <LinkedText key={index} text={node.text} />
      ) : (
        node.text
      )
    case 'emphasis':
      return <em key={index}>{children()}</em>
    case 'strong':
      return <strong key={index}>{children()}</strong>
    case 'strikethrough':
      return <del key={index}>{children()}</del>
    case 'code':
      return <code key={index}>{node.text}</code>
    case 'link':
      return (
        <Link key={index} target={node.target} title={node.title}>
          {children()}
        </Link>
      )
    case 'image':
      return node.source.kind === 'file' ? (
        <Picture
          key={index}
          path={node.source.path}
          description={node.description}
          title={node.title}
        />
      ) : (
        <Placeholder
          key={index}
          description={node.description}
          title={node.source.kind === 'web' ? node.source.url : node.source.source}
        />
      )
    case 'lineBreak':
      return <br key={index} />
  }
}

/** How long to wait before fetching again content whose code wasn't all highlighted. */
const HIGHLIGHT_RETRY_MS = 300

/**
 * Loads a preview's (or viewer's) content whenever it changes. While one load is running,
 * changes only mark it stale, so a stream of edits causes one load at a
 * time. Content whose code blocks weren't all highlighted in time is
 * fetched once more.
 */
export function useContent(id: string): PreviewContent | null {
  const version = useStore(previewVersions)[id] ?? 0
  const [content, setContent] = useState<PreviewContent | null>(null)
  const loading = useRef<{ id: string; stale: boolean } | null>(null)

  useEffect(() => {
    if (loading.current?.id === id) {
      loading.current.stale = true
      return
    }
    const run = { id, stale: false, retried: false }
    loading.current = run
    const load = (): Promise<void> =>
      getPreviewContent(id).then(async next => {
        if (loading.current !== run) {
          return
        }
        if (next) {
          setContent(next)
        }
        if (next?.highlightsPending && !run.retried && !run.stale) {
          run.retried = true
          await new Promise(resolve => setTimeout(resolve, HIGHLIGHT_RETRY_MS))
          run.stale = loading.current === run
        }
        if (run.stale) {
          run.stale = false
          return load()
        }
        if (loading.current === run) {
          loading.current = null
        }
      })
    load().catch((error: unknown) => {
      if (loading.current === run) {
        loading.current = null
      }
      reportError('Loading the preview', error)
    })
  }, [id, version])

  useEffect(
    () => () => {
      loading.current = null
    },
    [],
  )
  return content
}

export function PreviewPane({ preview }: { preview: PreviewInfo }) {
  return preview.webview ? (
    <WebviewFrame id={preview.id} title={preview.title} />
  ) : (
    <PreviewContentPane preview={preview} />
  )
}

function PreviewContentPane({ preview }: { preview: PreviewInfo }) {
  const content = useContent(preview.id)
  const loadImage = useCallback((path: string) => getPreviewImage(preview.id, path), [preview.id])
  return (
    <article className="preview" tabIndex={0} aria-label={preview.title}>
      <PreviewDocument content={content} loadImage={loadImage} />
    </article>
  )
}

interface PreviewDocumentProps {
  /** `null` while loading. */
  content: PreviewContent | null
  /** Loads a picture file the content shows; a new function loads them all again. */
  loadImage: (path: string) => Promise<string>
  /**
   * Web addresses and paths in the text are links too, opened with
   * Ctrl+click as elsewhere (`LinkedText`): for text written by others,
   * such as a language server's documentation.
   */
  linkText?: boolean
}

/**
 * Preview content drawn as elements, with its links followed and pictures
 * loaded: an extension's preview, an extension's README, or a hover card.
 */
export function PreviewDocument({ content, loadImage, linkText }: PreviewDocumentProps) {
  const tree = useMemo(
    () => (content ? { ...buildTree(content), linkText } : null),
    [content, linkText],
  )
  const ref = useRef<HTMLDivElement>(null)
  const images = useMemo(() => new Map<string, string>(), [loadImage])

  const context = useMemo<Context>(
    () => ({
      loadImage,
      images,
      follow: (target, choose = false) => {
        switch (target.kind) {
          // Through the link handlers, as links in the terminal are.
          case 'web':
            openLink({ kind: 'web', target: target.url }, choose)
            break
          case 'file':
            // `#L12` names a line, as on GitHub.
            openLink({ kind: 'file', target: target.path, line: anchorLine(target.anchor) }, choose)
            break
          case 'heading': {
            const heading = ref.current?.querySelector(
              `[data-anchor="${CSS.escape(target.anchor.toLowerCase())}"]`,
            )
            heading?.scrollIntoView({ block: 'start' })
            break
          }
          case 'unsupported':
            break
        }
      },
    }),
    [loadImage, images],
  )

  return (
    <div ref={ref} className="preview__document">
      <PreviewContext.Provider value={context}>
        {tree &&
          (tree.roots.length > 0 ? (
            tree.roots.map(index => renderNode(tree, index))
          ) : (
            <p className="preview__empty">Nothing to preview.</p>
          ))}
      </PreviewContext.Provider>
    </div>
  )
}
