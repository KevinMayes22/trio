# Trio

Trio is a Windows desktop app for running Three Amigos (refinement) sessions against Azure DevOps. It shows a sprint's
stories in a clean reading view, and once the people in the room have agreed the numbers it creates the Dev, Test and
Demo work items for the story in a single click.

This document describes what it does and how a session with it runs. For build instructions and implementation details,
see the [README](../README.md).

## Why it exists

A Three Amigos session brings together someone who owns the requirements, a developer and a tester to talk through a
story, agree what it means and estimate it. The conversation is the valuable part. The paperwork afterwards is not:
reading the description on one screen while the sprint board is open on another, then creating a Dev task, a Test case and
a Demo task by hand, copying the story's area and iteration onto each, linking them back to the story, typing in the
estimates, and leaving a note saying what was estimated. It is repetitive, easy to get slightly wrong, and it tends to eat
the end of the meeting.

Trio is meant to be used live in the room. You pick the story, read it together, type the hours and press Create.

## Running a session

Start by choosing the team and the sprint from the top bar. If you work across several teams you can switch at any time,
and Trio remembers the last team you used.

The list on the left shows the sprint's stories in the same order as the sprint board. Select one and its description,
acceptance criteria and screenshots appear in full. Screenshots in Azure DevOps normally sit behind a login, so Trio
fetches them for you and shows them inline. Existing discussion comments are tucked into a collapsed folder at the bottom.

If the conversation turns up a question or a request for changes, the Comment button posts it to the story's discussion, so
it lands on the ticket instead of being lost in the room.

When the estimates are agreed, the dock at the bottom has a row each for Dev, Test and Demo. For each one you pick who is
doing the work and type the hours, or leave it alone if that item isn't needed. Press Create and Trio makes the work
items, links them to the story and adds a summary comment. Then move on to the next story; small ticks beside each story in
the list show which ones have been dealt with.

## What gets created

| Row  | Work item type | Title                 | Notes                                                |
| ---- | -------------- | --------------------- | ---------------------------------------------------- |
| Dev  | Task           | `Dev - <story title>`  | Activity set to Development                          |
| Test | Test Case      | `Test - <story title>` | Created in the Design state                          |
| Demo | Task           | `Demo - <story title>` | Activity set to Testing. Off by default, 0.5 h ready |

Each item is a child of the story, sits in the same area and iteration as the story so it shows up on the sprint board, and
has its original estimate and remaining work set to the hours you entered, with completed work at zero. It is assigned to
whoever you chose, or left unassigned.

The story also gets a comment so the estimate is visible to anyone who opens it:

> **Trio Estimated Total : 4 Hrs**
>
> Dev : 2 hrs
> Test : 1.5 hrs
> Demo : 0.5 hrs
>
> 30 Oct 2026, 14:05

## Tickets that already exist

People often raise Dev, Test or Demo tickets ahead of a session while they prepare, so Trio looks for them rather than
creating duplicates. A ticket counts if it is a child of the story and its title starts with Dev, Test or Demo as a word,
in any case, so `Dev - ...`, `[Test] ...` and a plain `Test` all match. Removed items are ignored.

If the ticket already has hours, Trio treats it as finished. It says the item already exists, links to it, and never
touches it. If it has no hours, blank or zero, Trio treats it as a placeholder, tells you it found it, and offers to assign
the hours to it instead of creating a new one. A placeholder is only renamed when its title says nothing beyond the kind,
such as `Test`; a title like `Test - Accounts insert` is kept as written. Whatever it finds, you can still choose to create
a new item instead, or to leave the ticket alone.

## Other things worth knowing

- **Markdown descriptions.** Descriptions and acceptance criteria written in Markdown are rendered as formatted text
  rather than one long block of symbols.
- **Reordering.** Select a story and press Alt+Up or Alt+Down to move it. The new position is saved to the sprint backlog,
  so the board follows. That changes the order for everyone on the team.
- **Progress at a glance.** Each story in the list carries three small ticks for Dev, Test and Demo. A tick is lit when
  that item exists and has hours.
- **Nothing is created twice.** Once an item has been made, its row locks and shows a link to it.
- **Failures are reported per item.** Azure DevOps has no transactions, so if one item fails the others still stand, and
  the failed one can be retried on its own.
- **Demo mode.** Ticking "Use demo data" in the settings lets you try everything against sample stories. Nothing is sent
  anywhere.

## Privacy and security

The personal access token is stored in the Windows Credential Manager. The user interface never sees it, and it is only
ever sent to Azure DevOps. The front end makes no network requests of its own: a Rust backend does all the talking to
Azure DevOps, story HTML is sanitised before it is displayed, and screenshots are downloaded by the backend and embedded so
the page never contacts Azure DevOps directly.

## Requirements

| Requirement | Detail |
| ----------- | ------ |
| Platform    | Windows 10 or 11 (uses WebView2) |
| Azure DevOps | An organisation name and a project name |
| Token       | A personal access token with Work Items: Read & write, and Project and Team: Read |
| Process     | Test Cases need the three estimate fields (Original Estimate, Remaining Work, Completed Work). Without them Azure DevOps rejects the Test Case. |

On first launch Trio asks for the organisation, project and token, and then you choose a team. The installer is unsigned,
so Windows SmartScreen will warn the first time it runs.

## Status and limitations

Trio assumes the work item types `User Story`, `Technical Task`, `Task` and `Test Case`,
so other process templates need a small code change. Reordering uses an Azure DevOps API that has not yet been tried
against a wide range of organisations. Items created through Trio appear under the name of the token's owner.

It is built with Tauri v2, with a Rust backend and a plain HTML, CSS and JavaScript front end, and has around 90 automated
tests covering the work item payloads, title matching, the rules for existing tickets, Markdown handling, team listing and
the Azure DevOps client against a mock server. Releases are published at <https://github.com/KevinMayes22/trio/releases>.

Trio is released under the MIT licence.
