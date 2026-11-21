# ADR 0056: One side bar, ordered from the activity bar

- **Status:** Accepted
- **Date:** 2026-11-21

## Context

[ADR 0012](0012-docking.md) let anything be dropped on the activity bar to join the side bar, but
only among items of its kind. A terminal or a document dropped there got a new group below the
side bar's views, so every new kind split the side bar again. The order of the side bar's items
could only be changed by dragging their tabs, and its tab strip, with a tab per item, got crowded
as more were docked there.

## Decisions

### Drops on the activity bar join the side bar

An item dropped on the activity bar joins a side bar group, whatever its kind: among items of its
kind if a group has them, else in the first group showing, else the first group. Opening a view
that isn't docked uses the same rule. With no side bar, the item still gets a new group on the
left.

Splitting the side bar is still possible, from the dock: dropped near an edge of a side bar
group's content, an item gets a group beside it, as anywhere else.

### The activity bar orders the side bar

- **Where it's dropped.** A drag over the activity bar shows a line between the buttons it would
  drop between, and the item lands right after the button above the line (first, at the top).
  Dragging a button up or down reorders the side bar's tabs. Below the side bar's items, among
  views not docked anywhere, it lands last.
- **From the keyboard.** `Alt+Up` and `Alt+Down` on a button move its item one place
  (`moveInSidebar`), keeping focus on the button and the tab that shows. The buttons say so with
  `aria-keyshortcuts`.
- **Split side bars.** A separator divides the buttons of each side bar group. Dropped above
  its middle, an item becomes the last tab of the group above; below it, the first tab of the
  group below (`SidebarPlace`). The line shows under the upper group's last button or over the
  lower group's first, so the two are told apart. Moving an item past a separator from the
  keyboard puts it at the near end of the next group, so the keyboard can move items between
  groups too; a group left empty goes, which joins the side bar again.

### The side bar shows only its active tab

A side bar group's tab strip shows only its active tab, with its buttons, as a title. The other
items are reached from the activity bar beside it. The tab still takes drops, and Left and Right
on it still go through every item of the group. Groups elsewhere, including in other windows,
show all their tabs.

## Consequences

- **Fewer splits.** Layouts saved with a terminal or document group below the side bar's views
  keep it; only new drops join.
- **Changing a side bar group's tab without the activity bar** is by Left and Right on its tab,
  `Ctrl+Tab`, or the item's command.
