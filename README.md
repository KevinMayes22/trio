# Trio

A small desktop app for "Three Amigos" refinement sessions. It shows the stories in your team's sprint
(description, acceptance criteria and screenshots included), lets the team enter a Dev, Test and Demo estimate
and assignee, and creates the linked child work items in Azure DevOps. See the
[overview](docs/Trio-overview.md) for a walkthrough of a session.

Built with [Tauri v2](https://tauri.app): a Rust backend talks to Azure DevOps, a plain HTML/CSS/JS
frontend (no bundler) renders the UI.

## Running it

Prerequisites: [Rust](https://rustup.rs), Node.js, and on Windows the MSVC build tools and WebView2.

```bash
npm install
npm run tauri dev        # run with hot reload of the frontend
npm run tauri build      # produce an installer / exe in src-tauri/target/release
cargo test --manifest-path src-tauri/Cargo.toml
```

On first launch Trio asks for:

| Setting | Example |
| --- | --- |
| Organisation | `MyOrg` (or paste `https://dev.azure.com/MyOrg`) |
| Project | `My Project` |
| Personal access token | needs **Work Items → Read & write** and **Project and Team → Read** |

Organisation, project and the team you last chose are saved in `%APPDATA%\uk.co.kmayes.trio\settings.json`. The PAT is saved in the
Windows Credential Manager and never reaches the webview. For development you can set the `TRIO_PAT`
environment variable instead.

**Teams:** Trio lists the project's teams in the **Team** picker in the top bar, and shows nothing until you choose one.
The sprints, members (for the assignee lists) and board ordering all come from the chosen team. Switch team at any time from
the same picker; the choice is remembered for next launch, and the default assignees are remembered per team. Changing the
organisation or project in the settings clears the saved team.

Tick **Use demo data** in the settings to try the app with built-in sample stories, no PAT required. Nothing is
sent anywhere in demo mode; created tasks are logged to stderr.

## What it creates

For each story you give hours (and optionally an assignee) for Dev, Test and Demo. Dev becomes a `Task`, Test
becomes a `Test Case`, and Demo (the demo / sign-off) becomes a `Task`. The Demo row starts on **Do not create**
with 0.5 hours ready; pick a person (or "Unassigned") to create it. Each is:

- titled `Dev - <story title>` / `Test - <story title>` / `Demo - <story title>`
- linked as a child of the story (`System.LinkTypes.Hierarchy-Reverse`, shown as "Parent" on the new item)
- in the **same Area and Iteration Path as the story**, so it shows on the sprint board
- given **Original Estimate = Remaining Work = the entered hours**, and Completed Work `0`

They differ in Activity: the Dev Task gets `Development`, the Demo Task gets `Testing`, and a Test Case gets none
(it has no such field).

Test Cases need the three estimate fields (`Microsoft.VSTS.Scheduling.OriginalEstimate`, `RemainingWork`,
`CompletedWork`) on the `Test Case` type, as they are in a customised process. Without them Azure DevOps rejects
the Test Case and the Test row shows that error.

Type the hours in each row's box (Demo starts at 0.5). Choose **Do not create** in a row's assignee dropdown to skip that item (for example when the developer and
tester work independently). Whatever you pick in a row's dropdown, including "Do not create", is remembered for the
next story.

When you click **Create**, Trio also adds a comment to the story summarising what it just created, for example:

> **Trio Estimated Total : 4 Hrs**
>
> Dev : 2 hrs<br>Test : 1.5 hrs<br>Demo : 0.5 hrs
>
> 30 Oct 2026, 14:05

Only the items created by that click are listed and totalled, and the time is your PC's local time. If the comment
fails (for example the PAT can't comment) the items stay created and Trio says so.

**Existing items:** when a story opens, Trio reads its child work items and marks any Dev / Test / Demo row that
already has one as done ("already exists", with its state, link and Original Estimate), so nothing gets created twice.
An item counts if it is linked below the story and its title starts with `Dev`, `Test` or `Demo` as a word (`Dev -`,
`Test:`, `[DEV]`, plain `Test`, any case). Items in state `Removed` are ignored, and if a kind appears more than once the
oldest wins. Each story in the list shows three small ticks, one per kind in Dev / Test / Demo order, coloured like the
rows (yellow, cyan, orange) when the story already has that item with hours, and an empty faint circle when it hasn't.
They are worked out for the whole sprint when the list loads (two extra calls); hover a tick to see which kind it is.
**Create another** overrides it for the session. If the children cannot be read the story still opens, as if none existed.

**Placeholders:** an item with no Original Estimate (blank or zero) is a placeholder someone raised ahead of refinement.
Instead of "already exists" its row says it found the item and that hours will be assigned: on **Update** it gets the
hours entered (Original Estimate and Remaining Work). It is only renamed (to `Dev -` / `Test -` / `Demo -` plus the story
title) when its title says nothing beyond the kind, such as `Test`; a title like `Test - Accounts insert` is kept. The
assignee is only changed if one is picked, and the state, area, iteration and links are left alone. **Don't update** skips the
item. **Create new instead** creates a fresh item as usual. An item
that already has hours is treated as done and is never touched. New Test Cases are created in state `Design`.

**Markdown fields:** a Description or Acceptance Criteria set to the Markdown format in Azure DevOps comes back as raw
Markdown; it is rendered to HTML (text with no HTML block tags is taken to be Markdown).

Azure DevOps has no transactions, so tasks are created one at a time and each reports its own result. Once a task
is created its row is locked (with a link to it) so a second click can't create a duplicate; "Create another"
unlocks it. This "done" marker is remembered locally per story.

Existing Discussion comments are listed in a **Comments** folder at the bottom of the story. It starts collapsed
with a count next to it. Reading comments uses a preview API (`7.1-preview.4`); if that call ever fails the story
still opens and the folder shows the error.

The **Comment** button posts a note to the story's Discussion (by setting `System.History`), for questions or
requested changes that come up in the session. It appears under the PAT owner's name. Unsent text is kept as a draft
per story.

**Reordering:** select a story and press **Alt+↑ / Alt+↓** (or use the ▲▼ buttons on the selected story) to move it
up or down. The list moves immediately and the new position is saved to the sprint backlog in Azure DevOps, so the
sprint board shows the same order. It affects the whole team's board. Clear the filter first; reordering is off
while it is active. If a save fails, the list reloads from Azure DevOps and a message explains why. This uses the
"Reorder Sprint Backlog/Taskboard Work Items" API (`PATCH .../_apis/work/iterations/{id}/workitemsorder`), which has not
been tried against a real organisation yet.

The list is in the same order as the sprint board. Trio reads that order from the sprint's own work item list
(`GET .../_apis/work/teamsettings/iterations/{id}/workitems`); if that call fails it falls back to the query's backlog
order, which can differ between process templates.

The list shows `User Story` items (grey book icon) and `Technical Task` items (green cog icon) in state `New` or
`Active` in the selected sprint, in backlog order, and both can be broken down into Dev and Test tasks. `Technical
Task` is the work item type name (`TECHNICAL_TASK_TYPE` in `src-tauri/src/model.rs`). If a project has no such
type, Azure DevOps rejects the query and Trio quietly falls back to listing user stories only. Change
`wiql_for_iteration` in `src-tauri/src/payload.rs` for other process templates.

## Architecture rule

All functionality lives in the Rust backend (Tauri commands). The frontend only renders and calls
`invoke(...)`; it makes no network requests of its own (the CSP also blocks them), never sees the PAT, and has no
framework or bundler. New features should be added as a backend command first, then wired to the UI.

## Layout

```
src/                  frontend: index.html, styles.css, main.js
src-tauri/src/
  ado.rs              Azure DevOps REST client (api-version 7.1) + response parsing
  payload.rs          task JSON Patch + WIQL building (pure, unit tested)
  html.rs             sanitising story HTML and inlining screenshots
  service.rs          story rendering and task creation, on top of a Backend
  backend.rs          enum: real Azure DevOps or the offline demo
  demo.rs             built-in sample data
  settings.rs         settings file + Credential Manager PAT
  lib.rs              Tauri commands and app state
```

Screenshots in stories sit behind authentication, so the backend downloads them with the PAT and embeds them as
`data:` URIs. The PAT is only ever sent to `dev.azure.com` / `*.visualstudio.com` attachment URLs, and the
webview's CSP only allows `self` and `data:` images.

## Licence

[MIT](LICENSE)
