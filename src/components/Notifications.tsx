import { useEffect, useState } from 'react'
import { dismiss, notifications, reportError, type Notification } from '../state/notifications'
import { useStore } from '../state/store'
import { LinkedText } from './LinkedText'
import './Notifications.css'

/** How long a notification shows; warnings and errors take longer to read. */
const AUTO_DISMISS_MS = { info: 10_000, warning: 20_000, error: 20_000 } as const

export function Notifications() {
  const items = useStore(notifications)
  return (
    <div className="notifications" aria-live="polite">
      {items.map(item => (
        <NotificationItem key={item.id} item={item} />
      ))}
    </div>
  )
}

/**
 * Notifications disappear after a while, but not while the pointer or focus
 * is on them; the Problems view keeps warnings and errors. Sticky ones (a
 * question only they ask) stay until answered or dismissed.
 */
function NotificationItem({ item }: { item: Notification }) {
  const sticky = item.sticky ?? false
  const [hovered, setHovered] = useState(false)
  const [focused, setFocused] = useState(false)
  const paused = hovered || focused
  useEffect(() => {
    if (sticky || paused) {
      return
    }
    const timer = setTimeout(() => dismiss(item.id), AUTO_DISMISS_MS[item.kind])
    return () => clearTimeout(timer)
  }, [item.id, item.kind, sticky, paused])

  return (
    <div
      className="notification"
      data-kind={item.kind}
      role={item.kind === 'error' ? 'alert' : 'status'}
      onMouseEnter={() => setHovered(true)}
      onMouseLeave={() => setHovered(false)}
      onFocus={() => setFocused(true)}
      onBlur={event => {
        if (!event.currentTarget.contains(event.relatedTarget)) {
          setFocused(false)
        }
      }}
    >
      <div className="notification__body">
        <p className="notification__message">
          <LinkedText text={item.message} />
        </p>
        {(item.actions?.length ?? 0) > 0 && (
          <div className="notification__actions">
            {item.actions?.map(action => (
              <button
                key={action.label}
                type="button"
                className="notification__action"
                onClick={() => {
                  dismiss(item.id)
                  Promise.resolve(action.run()).catch((error: unknown) =>
                    reportError(action.label, error),
                  )
                }}
              >
                {action.label}
              </button>
            ))}
          </div>
        )}
      </div>
      <button
        type="button"
        className="notification__dismiss"
        aria-label="Dismiss notification"
        onClick={() => dismiss(item.id)}
      >
        ×
      </button>
    </div>
  )
}
