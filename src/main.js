const { invoke } = window.__TAURI__.core;

const $ = (selector) => document.querySelector(selector);
const el = {
  team: $("#team"),
  sprint: $("#sprint"),
  sprintDates: $("#sprint-dates"),
  demoBadge: $("#demo-badge"),
  reload: $("#reload"),
  openSettings: $("#open-settings"),
  filter: $("#filter"),
  list: $("#story-list"),
  detailBody: $("#detail-body"),
  dock: $("#dock"),
  settings: $("#settings"),
  lightbox: $("#lightbox"),
  commentDialog: $("#comment-dialog"),
  commentText: $("#comment-text"),
  commentError: $("#comment-error"),
  commentPost: $("#comment-post"),
  toast: $("#toast"),
};

const state = {
  settings: null,
  teams: [],
  iterations: [],
  members: [],
  stories: [],
  selectedId: null,
  detail: null,
  filter: "",
  creating: false,
  rowErrors: {},
  // Dev / Test / Demo items Azure DevOps already has under each story, once it has been opened.
  existing: {},
  // "storyId:kind" for existing items the user chose to create another of.
  dismissed: {},
};

// What each row creates. `hours` is pre-filled and `skip` starts the row on "Do not create" (until the
// user picks something, which is then remembered for every story).
const KINDS = [
  { kind: "dev", label: "Dev", role: "Developer" },
  { kind: "test", label: "Test", role: "Tester" },
  { kind: "demo", label: "Demo", role: "Demo / sign-off", hours: "0.5", skip: true },
];

// ---------- helpers ----------

const esc = (value) =>
  String(value ?? "").replace(/[&<>"']/g, (c) => ({ "&": "&amp;", "<": "&lt;", ">": "&gt;", '"': "&quot;", "'": "&#39;" })[c]);

const errorText = (error) => (typeof error === "string" ? error : error?.message ?? String(error));

const readJson = (key, fallback) => {
  try {
    return JSON.parse(localStorage.getItem(key)) ?? fallback;
  } catch {
    return fallback;
  }
};
const writeJson = (key, value) => localStorage.setItem(key, JSON.stringify(value));
const draftKey = (id) => `trio.draft.${id}`;
// The same people usually cover every story in a session, so the last developer and tester
// picked become the starting point for the next story.
const DEFAULTS_KEY = "trio.defaultAssignees";
// People differ between teams, so the defaults are kept per team (the one list from before teams
// could be switched is still used until a team has its own).
const defaultsKey = () => `${DEFAULTS_KEY}.${state.settings?.team ?? ""}`;
const readDefaults = () => readJson(defaultsKey(), null) ?? readJson(DEFAULTS_KEY, {});
const doneKey = (id) => `trio.done.${id}`;
const commentDraftKey = (id) => `trio.comment.${id}`;

// Items somebody raised by hand with no hours yet: Trio offers to rename and estimate these
// instead of creating new ones, unless the user chose to create another. Items with hours are done.
function adoptables(storyId) {
  const found = {};
  for (const item of state.existing[storyId] ?? []) {
    if (item.adoptable && !state.dismissed[`${storyId}:${item.kind}`]) found[item.kind] = item;
  }
  return found;
}

// The items to show as done for a story: those Trio created (remembered locally), overridden by
// what Azure DevOps reports under the story, which is the truth when someone made them elsewhere.
function doneItems(storyId) {
  const done = { ...readJson(doneKey(storyId), {}) };
  const adopt = adoptables(storyId);
  for (const item of state.existing[storyId] ?? []) {
    if (state.dismissed[`${storyId}:${item.kind}`]) continue;
    if (adopt[item.kind]) {
      delete done[item.kind];
      continue;
    }
    done[item.kind] = { ...item, hours: item.hours ?? done[item.kind]?.hours ?? null, found: true };
  }
  return done;
}

const formatHours = (hours) => (Number.isInteger(hours) ? String(hours) : hours.toFixed(2).replace(/\.?0+$/, ""));

let toastTimer;
function toast(message) {
  el.toast.textContent = message;
  el.toast.classList.add("show");
  clearTimeout(toastTimer);
  toastTimer = setTimeout(() => el.toast.classList.remove("show"), 2800);
}

const openExternal = (url) => window.__TAURI__.opener.openUrl(url);

// One tick per kind in the story list: coloured when the story already has that item, faint when not.
function itemTicks(items = []) {
  return `<span class="item-ticks">${KINDS.map(({ kind, label }) => {
    const has = items.includes(kind);
    return `<span class="item-tick ${kind} ${has ? "has" : ""}" title="${label}: ${has ? "already created" : "none yet"}">${has ? CHECK_ICON : ""}</span>`;
  }).join("")}</span>`;
}

const CHECK_ICON = `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M5 12.5l4.5 4.5L19 7.5"/></svg>`;

// ---------- loading ----------

async function init() {
  state.settings = await invoke("get_settings");
  renderBadge();
  if (state.settings.configured) {
    await loadAll();
  } else {
    renderListNote("Connect to Azure DevOps to see your sprint.", { settings: true });
    openSettings();
  }
}

async function loadAll() {
  renderListSkeleton();
  el.detailBody.innerHTML = "";
  el.dock.hidden = true;
  el.reload.classList.add("spinning");
  try {
    state.teams = await invoke("list_teams");
    renderTeamPicker();
    if (!currentTeam()) {
      showNoTeam();
      return;
    }
    await loadTeamData();
  } catch (error) {
    renderListNote(errorText(error), { error: true, settings: true });
    el.detailBody.innerHTML = "";
  } finally {
    el.reload.classList.remove("spinning");
  }
}

// The saved team, if the project still has it.
const currentTeam = () => state.teams.find((t) => t.name === state.settings?.team)?.name ?? "";

function renderTeamPicker() {
  const current = currentTeam();
  el.team.innerHTML =
    (current ? "" : `<option value="" selected disabled>Choose a team…</option>`) +
    state.teams.map((t) => `<option value="${esc(t.name)}" ${t.name === current ? "selected" : ""}>${esc(t.name)}</option>`).join("");
  el.team.disabled = state.teams.length === 0;
}

// Nothing to show until a team has been picked: its sprints, members and ordering all come from it.
function showNoTeam() {
  state.iterations = [];
  state.members = [];
  state.stories = [];
  state.selectedId = null;
  renderSprintPicker();
  el.dock.hidden = true;
  renderListNote(state.teams.length === 0 ? "This project has no teams." : "Choose a team to see its sprints.");
  el.detailBody.innerHTML = `<div class="empty-state"><p>${state.teams.length === 0 ? "No teams found." : "Choose your team in the top bar."}</p></div>`;
}

// The current team's sprints and members, then its stories.
async function loadTeamData() {
  const team = state.settings.team;
  const [iterations, members] = await Promise.all([invoke("list_iterations"), invoke("list_members")]);
  if (state.settings.team !== team) return; // another team was picked while this was loading
  state.iterations = iterations;
  state.members = members;
  renderSprintPicker();
  await loadStories();
}

async function changeTeam(name) {
  const previous = state.settings.team;
  try {
    state.settings = await invoke("set_team", { team: name });
  } catch (error) {
    toast(`Could not switch team: ${errorText(error)}`);
    renderTeamPicker();
    return;
  }
  state.selectedId = null;
  state.detail = null;
  state.stories = [];
  state.iterations = [];
  state.members = [];
  renderSprintPicker();
  renderListSkeleton();
  el.detailBody.innerHTML = "";
  el.dock.hidden = true;
  el.reload.classList.add("spinning");
  try {
    await loadTeamData();
  } catch (error) {
    if (state.settings.team !== name) return;
    renderListNote(errorText(error), { error: true });
    toast(previous === name ? "Could not load the team" : `Could not load ${name}`);
  } finally {
    el.reload.classList.remove("spinning");
  }
}

function pickDefaultIteration(iterations) {
  return (
    iterations.find((i) => i.timeFrame === "current") ??
    iterations.find((i) => i.timeFrame === "future") ??
    iterations[iterations.length - 1]
  );
}

function renderSprintPicker() {
  const previous = el.sprint.value;
  el.sprint.innerHTML = state.iterations
    .map((i) => `<option value="${esc(i.path)}">${esc(i.name)}${i.timeFrame === "current" ? " (current)" : ""}</option>`)
    .join("");
  el.sprint.disabled = state.iterations.length === 0;
  const chosen = state.iterations.find((i) => i.path === previous) ?? pickDefaultIteration(state.iterations);
  if (chosen) el.sprint.value = chosen.path;
  renderSprintDates();
}

const selectedIteration = () => state.iterations.find((i) => i.path === el.sprint.value);

function renderSprintDates() {
  const iteration = selectedIteration();
  const short = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short" });
  el.sprintDates.textContent =
    iteration?.startDate && iteration?.finishDate
      ? `${short.format(new Date(iteration.startDate))} – ${short.format(new Date(iteration.finishDate))}`
      : "";
}

async function loadStories() {
  const path = el.sprint.value;
  renderSprintDates();
  if (!path) {
    state.stories = [];
    renderListNote("This team has no sprints selected in Azure DevOps.");
    return;
  }
  renderListSkeleton();
  const team = state.settings.team;
  try {
    const stories = await invoke("list_stories", { iterationId: selectedIteration()?.id ?? "", iterationPath: path });
    if (state.settings.team !== team) return; // another team was picked meanwhile
    state.stories = stories;
  } catch (error) {
    if (state.settings.team === team) renderListNote(errorText(error), { error: true });
    return;
  }
  renderList();
  const keep = state.stories.find((s) => s.id === state.selectedId);
  const first = visibleStories()[0];
  if (keep) await selectStory(keep.id);
  else if (first) await selectStory(first.id);
  else {
    state.selectedId = null;
    renderDetailEmpty();
  }
}

// ---------- story list ----------

function visibleStories() {
  const needle = state.filter.trim().toLowerCase();
  if (!needle) return state.stories;
  return state.stories.filter((s) => `${s.id} ${s.title} ${s.tags.join(" ")} ${s.assignedTo ?? ""}`.toLowerCase().includes(needle));
}

function renderListSkeleton() {
  el.list.innerHTML = `<div class="skeleton"></div><div class="skeleton"></div><div class="skeleton"></div><div class="skeleton"></div>`;
}

function renderListNote(message, { error = false, settings = false } = {}) {
  el.list.innerHTML = `
    <div class="list-note">
      ${error ? `<p class="error">${esc(message)}</p>` : `<p>${esc(message)}</p>`}
      ${settings ? `<p style="margin-top:12px"><button class="secondary" data-action="settings">Open settings</button></p>` : ""}
    </div>`;
}

const KIND_ICONS = {
  story: `<svg viewBox="0 0 24 24" aria-hidden="true"><path d="M2 3h6a4 4 0 0 1 4 4v14a3 3 0 0 0-3-3H2z"/><path d="M22 3h-6a4 4 0 0 0-4 4v14a3 3 0 0 1 3-3h7z"/></svg>`,
  technical: `<svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="12" cy="12" r="3"/><path d="M19.4 15a1.7 1.7 0 0 0 .3 1.8l.1.1a2 2 0 1 1-2.8 2.8l-.1-.1a1.7 1.7 0 0 0-1.8-.3 1.7 1.7 0 0 0-1 1.5V21a2 2 0 1 1-4 0v-.1a1.7 1.7 0 0 0-1.1-1.5 1.7 1.7 0 0 0-1.8.3l-.1.1a2 2 0 1 1-2.8-2.8l.1-.1a1.7 1.7 0 0 0 .3-1.8 1.7 1.7 0 0 0-1.5-1H3a2 2 0 1 1 0-4h.1a1.7 1.7 0 0 0 1.5-1.1 1.7 1.7 0 0 0-.3-1.8l-.1-.1a2 2 0 1 1 2.8-2.8l.1.1a1.7 1.7 0 0 0 1.8.3H9a1.7 1.7 0 0 0 1-1.5V3a2 2 0 1 1 4 0v.1a1.7 1.7 0 0 0 1 1.5 1.7 1.7 0 0 0 1.8-.3l.1-.1a2 2 0 1 1 2.8 2.8l-.1.1a1.7 1.7 0 0 0-.3 1.8V9a1.7 1.7 0 0 0 1.5 1H21a2 2 0 1 1 0 4h-.1a1.7 1.7 0 0 0-1.5 1z"/></svg>`,
};
const KIND_LABELS = { story: "User Story", technical: "Technical Task" };
const kindIcon = (kind) => `<span class="kind-icon kind-${kind}" title="${KIND_LABELS[kind]}">${KIND_ICONS[kind]}</span>`;

const moveControls = () => `
  <span class="move" aria-label="Move story">
    <span class="move-btn" role="button" data-move="-1" title="Move up (Alt+↑)" aria-label="Move up"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 15l6-6 6 6"/></svg></span>
    <span class="move-btn" role="button" data-move="1" title="Move down (Alt+↓)" aria-label="Move down"><svg viewBox="0 0 24 24" aria-hidden="true"><path d="M6 9l6 6 6-6"/></svg></span>
  </span>`;

function renderList() {
  const stories = visibleStories();
  const movable = !state.filter.trim();
  if (stories.length === 0) {
    renderListNote(state.stories.length === 0 ? "No New or Active stories or technical tasks in this sprint." : "No stories match your filter.");
    return;
  }
  el.list.innerHTML = stories
    .map((story) => {
      const meta = [story.assignedTo, ...story.tags].filter(Boolean).join(" · ");
      return `
        <button class="story" role="option" data-id="${story.id}" aria-selected="${story.id === state.selectedId}">
          <span class="story-top">
            ${kindIcon(story.kind)}
            <span class="id">#${story.id}</span>
            <span class="chip state-${esc(story.state.toLowerCase())}">${esc(story.state)}</span>
            ${story.storyPoints != null ? `<span class="chip points">${esc(story.storyPoints)} pts</span>` : ""}
          </span>
          <span class="story-title">${esc(story.title)}</span>
          ${meta ? `<span class="story-meta">${esc(meta)}</span>` : ""}
          ${itemTicks(story.items)}
          ${movable ? moveControls() : ""}
        </button>`;
    })
    .join("");
}

// Moves the selected story one place up (-1) or down (+1). The list updates straight away and the
// backend saves the new position; requests are queued so quick key presses stay in order.
let moveQueue = Promise.resolve();

function moveSelected(delta) {
  if (state.filter.trim()) {
    toast("Clear the filter to reorder stories");
    return;
  }
  const from = state.stories.findIndex((s) => s.id === state.selectedId);
  const to = from + delta;
  if (from < 0 || to < 0 || to >= state.stories.length) return;

  const iteration = state.iterations.find((i) => i.path === el.sprint.value);
  if (!iteration) return;

  const stories = [...state.stories];
  const [story] = stories.splice(from, 1);
  stories.splice(to, 0, story);
  state.stories = stories;
  const request = { iterationId: iteration.id, iterationPath: iteration.path, id: story.id, previousId: stories[to - 1]?.id ?? null, nextId: stories[to + 1]?.id ?? null };
  renderList();
  markSelected();

  moveQueue = moveQueue.then(async () => {
    try {
      await invoke("move_story", request);
    } catch (error) {
      toast(`Could not move the story: ${errorText(error)}`);
      if (el.sprint.value === iteration.path) await resyncStories(iteration.path);
    }
  });
}

// Reloads the order from Azure DevOps (after a failed move) without disturbing the open story.
async function resyncStories(path) {
  try {
    const stories = await invoke("list_stories", { iterationId: selectedIteration()?.id ?? "", iterationPath: path });
    if (el.sprint.value !== path) return;
    state.stories = stories;
    renderList();
    markSelected();
  } catch (error) {
    toast(errorText(error));
  }
}

function markSelected() {
  for (const item of el.list.querySelectorAll(".story")) {
    item.setAttribute("aria-selected", String(Number(item.dataset.id) === state.selectedId));
  }
  el.list.querySelector('.story[aria-selected="true"]')?.scrollIntoView({ block: "nearest" });
}

// ---------- story detail ----------

function renderDetailEmpty() {
  state.detail = null;
  el.dock.hidden = true;
  el.detailBody.innerHTML = `<div class="empty-state"><p>Select a story to start refining.</p></div>`;
}

async function selectStory(id) {
  state.selectedId = id;
  state.rowErrors = {};
  markSelected();
  el.detailBody.innerHTML = `<div class="detail-inner"><div class="skeleton" style="height:120px"></div><div class="skeleton" style="height:200px"></div></div>`;
  el.dock.hidden = true;
  try {
    const detail = await invoke("get_story", { id });
    if (state.selectedId !== id) return; // user moved on while this was loading
    state.detail = detail;
    state.existing[id] = detail.existing;
    renderDetail();
    renderDock();
    // Opening the story re-reads its children, so the tick in the list is brought up to date.
    const listed = state.stories.find((s) => s.id === id);
    const found = detail.existing.filter((item) => !item.adoptable).map((item) => item.kind);
    if (listed && listed.items.join() !== found.join()) {
      listed.items = found;
      renderList();
    }
  } catch (error) {
    if (state.selectedId !== id) return;
    state.detail = null;
    el.detailBody.innerHTML = `<div class="detail-inner"><p class="error">${esc(errorText(error))}</p></div>`;
  }
}

function richSection(title, html, emptyText) {
  return `
    <section class="section">
      <h3>${title}</h3>
      ${html ? `<div class="rich">${html}</div>` : `<p class="none">${emptyText}</p>`}
    </section>`;
}

const commentDate = new Intl.DateTimeFormat("en-GB", { day: "numeric", month: "short", year: "numeric", hour: "2-digit", minute: "2-digit" });
const initials = (name) =>
  name.split(/\s+/).filter(Boolean).slice(0, 2).map((part) => part[0].toUpperCase()).join("") || "?";

function commentHtml(comment) {
  const when = comment.created ? commentDate.format(new Date(comment.created)) : "Just now";
  return `
    <article class="comment">
      <div class="avatar" aria-hidden="true">${esc(initials(comment.author))}</div>
      <div class="comment-main">
        <header><b>${esc(comment.author)}</b><time>${esc(when)}</time></header>
        <div class="rich">${comment.html}</div>
      </div>
    </article>`;
}

// A collapsed "folder": only the count is visible until the user expands it.
function commentsHtml(comments, error, open) {
  const body = error
    ? `<p class="error">Couldn't load the comments: ${esc(error)}</p>`
    : comments.length === 0
      ? `<p class="none">No comments yet.</p>`
      : comments.map(commentHtml).join("");
  return `
    <details class="section comments" id="comments" ${open ? "open" : ""}>
      <summary>
        <svg class="chevron" viewBox="0 0 24 24" aria-hidden="true"><path d="M9 6l6 6-6 6"/></svg>
        <span>Comments</span>
        <span class="count ${error ? "count-error" : ""}" title="${error ? "Could not be loaded" : `${comments.length} comment${comments.length === 1 ? "" : "s"}`}">${error ? "!" : comments.length}</span>
      </summary>
      <div class="comment-list">${body}</div>
    </details>`;
}

async function refreshComments(storyId, { open = false } = {}) {
  try {
    const comments = await invoke("get_comments", { storyId });
    if (state.selectedId !== storyId || !state.detail) return;
    state.detail.comments = comments;
    const folder = $("#comments");
    if (!folder) return;
    folder.outerHTML = commentsHtml(comments, null, open || folder.open);
    if (open) $("#comments").scrollIntoView({ block: "nearest", behavior: "smooth" });
  } catch (error) {
    toast("Posted, but couldn't refresh the comments");
  }
}

function renderDetail() {
  const story = state.detail;
  const iterationName = story.iterationPath.split("\\").pop();
  el.detailBody.innerHTML = `
    <div class="detail-inner">
      <div class="story-head">
        ${kindIcon(story.kind)}
        <a class="id-link" href="${esc(story.webUrl)}" title="Open in Azure DevOps">#${story.id}</a>
        <span class="chip state-${esc(story.state.toLowerCase())}">${esc(story.state)}</span>
        ${story.storyPoints != null ? `<span class="chip points">${esc(story.storyPoints)} pts</span>` : ""}
        ${story.tags.map((tag) => `<span class="chip tag">${esc(tag)}</span>`).join("")}
      </div>
      <h1 class="story-title-lg">${esc(story.title)}</h1>
      <p class="story-sub">${esc(iterationName)} · ${esc(story.areaPath)}${story.assignedTo ? ` · Assigned to ${esc(story.assignedTo)}` : ""}</p>
      ${richSection("Description", story.descriptionHtml, "No description.")}
      ${richSection("Acceptance criteria", story.acceptanceHtml, "No acceptance criteria.")}
      ${commentsHtml(story.comments, story.commentsError, false)}
    </div>`;
  el.detailBody.scrollTop = 0;
}

// ---------- estimate dock ----------

const SKIP = "__skip__";

function memberOptions(selected, adopting) {
  return (
    `<option value="${SKIP}" ${selected === SKIP ? "selected" : ""}>${adopting ? "Don’t update" : "Do not create"}</option>` +
    `<option value="" ${!selected ? "selected" : ""}>${adopting ? "Keep assignee" : "Unassigned"}</option>` +
    state.members
      .map((m) => `<option value="${esc(m.uniqueName)}" ${m.uniqueName === selected ? "selected" : ""}>${esc(m.displayName)}</option>`)
      .join("")
  );
}

function rowHtml({ kind, label, role, hours = "", skip = false }, draft, done, defaults, adopt) {
  if (done[kind]) {
    const item = done[kind];
    const type = item.workItemType ?? (kind === "test" ? "Test Case" : "Task");
    const status = item.found ? `already exists${item.state ? ` (${esc(item.state)})` : ""}` : "created";
    return `
      <div class="row ${kind} is-done">
        <div class="row-title">${label}</div>
        <div class="done-line">
          <span>✓ <a href="${esc(item.url)}">${esc(type)} #${item.id}</a> ${status}${item.hours != null ? ` · ${formatHours(item.hours)} h` : ""}</span>
          <button class="link-btn" data-again="${kind}">Create another</button>
        </div>
      </div>`;
  }
  // A found item is worth updating, so a remembered "Do not create" does not apply to it.
  const remembered = adopt && defaults[kind] === SKIP ? undefined : defaults[kind];
  const wanted = draft[`${kind}Assignee`] ?? remembered ?? (skip && !adopt ? SKIP : "");
  // Somebody picked for another team may not be in this one's list.
  const assignee = wanted === SKIP || state.members.some((m) => m.uniqueName === wanted) ? wanted : "";
  const skipped = assignee === SKIP;
  const startHours = adopt?.hours != null ? String(adopt.hours) : hours;
  return `
    <div class="row ${kind} ${skipped ? "is-skipped" : ""}" data-row="${kind}">
      <div class="row-title">${label}</div>
      <div class="row-fields">
        <select data-assignee="${kind}" aria-label="${role}">${memberOptions(assignee, Boolean(adopt))}</select>
        <label class="hours-label" for="hours-${kind}">Hours</label>
        <div class="input-wrap">
          <input id="hours-${kind}" type="number" min="0" max="999" step="0.5" inputmode="decimal" placeholder="0" ${skipped ? "disabled" : ""}
                 data-hours="${kind}" value="${esc(draft[`${kind}Hours`] ?? startHours)}" />
        </div>
      </div>
      ${
        adopt
          ? `<p class="row-note">Found ${esc(adopt.workItemType)} <a href="${esc(adopt.url)}">#${adopt.id}</a> “${esc(adopt.title)}”${adopt.state ? ` (${esc(adopt.state)})` : ""}.
               Hours will be assigned. <button class="link-btn" data-again="${kind}">Create new instead</button></p>`
          : ""
      }
      ${state.rowErrors[kind] ? `<p class="row-error">${esc(state.rowErrors[kind])}</p>` : ""}
    </div>`;
}

function renderDock() {
  const story = state.detail;
  if (!story) {
    el.dock.hidden = true;
    return;
  }
  const draft = readJson(draftKey(story.id), {});
  const done = doneItems(story.id);
  const adopt = adoptables(story.id);
  const defaults = readDefaults();
  el.dock.hidden = false;
  el.dock.innerHTML = `
    <div class="dock-inner">
      <div class="dock-head">
        <h2>Estimate &amp; assign</h2>
        <div class="spacer"></div>
        <span class="total" id="total"></span>
        <button class="secondary ${readJson(commentDraftKey(story.id), "") ? "has-draft" : ""}" id="open-comment" title="Ask a question or request changes on the story">Comment</button>
        <button class="primary" id="create"></button>
      </div>
      <div class="rows">${KINDS.map((k) => rowHtml(k, draft, done, defaults, adopt[k.kind])).join("")}</div>
    </div>`;
  refreshDockSummary();
}

function pendingTasks() {
  const done = doneItems(state.selectedId);
  const adopt = adoptables(state.selectedId);
  return KINDS.filter(({ kind }) => !done[kind])
    .map(({ kind }) => ({
      kind,
      adoptId: adopt[kind]?.id ?? null,
      hours: parseFloat(el.dock.querySelector(`[data-hours="${kind}"]`)?.value),
      assignee: el.dock.querySelector(`[data-assignee="${kind}"]`)?.value || null,
    }))
    .filter((task) => task.assignee !== SKIP && task.hours > 0);
}

function refreshDockSummary() {
  const done = doneItems(state.selectedId);
  const doneHours = Object.values(done).reduce((sum, d) => sum + (d.hours ?? 0), 0);
  const pending = pendingTasks();
  const total = doneHours + pending.reduce((sum, t) => sum + t.hours, 0);
  $("#total").innerHTML = `Total <b>${formatHours(total)}</b> h`;
  const button = $("#create");
  button.disabled = state.creating || pending.length === 0;
  const verb = pending.length > 0 && pending.every((t) => t.adoptId) ? "Update" : "Create";
  button.textContent = state.creating ? "Working…" : pending.length === 0 ? "Create tasks" : `${verb} ${pending.length} task${pending.length > 1 ? "s" : ""}`;
}

function saveDraft() {
  const draft = {};
  for (const { kind } of KINDS) {
    const hours = el.dock.querySelector(`[data-hours="${kind}"]`);
    const assignee = el.dock.querySelector(`[data-assignee="${kind}"]`);
    if (hours) draft[`${kind}Hours`] = hours.value;
    if (assignee) draft[`${kind}Assignee`] = assignee.value;
  }
  writeJson(draftKey(state.selectedId), { ...readJson(draftKey(state.selectedId), {}), ...draft });
}

async function createTasks() {
  const tasks = pendingTasks();
  if (state.creating || tasks.length === 0) return;
  const storyId = state.selectedId;
  state.creating = true;
  state.rowErrors = {};
  refreshDockSummary();

  try {
    // The backend also leaves a "Trio Estimated Total" comment on the story.
    const { results, commentError } = await invoke("create_tasks", { request: { storyId, tasks } });
    const done = readJson(doneKey(storyId), {});
    for (const result of results) {
      if (result.ok) {
        const hours = tasks.find((t) => t.kind === result.kind).hours;
        done[result.kind] = { id: result.id, url: result.url, hours };
        // What was found before is out of date now; the new item takes its place.
        state.existing[storyId] = (state.existing[storyId] ?? []).filter((item) => item.kind !== result.kind);
        delete state.dismissed[`${storyId}:${result.kind}`];
      } else {
        state.rowErrors[result.kind] = result.error;
      }
    }
    writeJson(doneKey(storyId), done);
    const created = results.filter((r) => r.ok).length;
    if (created > 0) {
      const listed = state.stories.find((s) => s.id === storyId);
      if (listed) {
        const made = results.filter((r) => r.ok).map((r) => r.kind);
        listed.items = KINDS.map((k) => k.kind).filter((kind) => listed.items.includes(kind) || made.includes(kind));
      }
      const updated = results.filter((r) => r.ok && r.adopted).length;
      toast(
        updated === 0
          ? `${created} item${created > 1 ? "s" : ""} created`
          : `${created - updated ? `${created - updated} created, ` : ""}${updated} existing item${updated > 1 ? "s" : ""} updated`,
      );
      if (state.selectedId === storyId) refreshComments(storyId, { open: true });
    }
    if (results.some((r) => !r.ok)) toast("Some items failed — see the details below");
    if (commentError) toast(`Created, but the estimate comment could not be added: ${commentError}`);
  } catch (error) {
    for (const task of tasks) state.rowErrors[task.kind] = errorText(error);
  } finally {
    state.creating = false;
    if (state.selectedId === storyId) renderDock();
    renderList();
  }
}

// ---------- comments ----------

let commentStoryId = null;

function openComment() {
  const story = state.detail;
  if (!story) return;
  commentStoryId = story.id;
  $("#comment-title").textContent = `Comment on #${story.id}`;
  el.commentText.value = readJson(commentDraftKey(story.id), "");
  el.commentError.classList.add("hidden");
  el.commentDialog.showModal();
  el.commentText.focus();
}

function updateCommentDraft() {
  const text = el.commentText.value;
  if (text.trim()) writeJson(commentDraftKey(commentStoryId), text);
  else localStorage.removeItem(commentDraftKey(commentStoryId));
  if (state.selectedId === commentStoryId) $("#open-comment")?.classList.toggle("has-draft", Boolean(text.trim()));
}

async function postComment() {
  if (el.commentPost.disabled) return;
  const storyId = commentStoryId;
  el.commentPost.disabled = true;
  el.commentPost.textContent = "Posting…";
  el.commentError.classList.add("hidden");
  try {
    await invoke("add_comment", { storyId, text: el.commentText.value });
    el.commentText.value = "";
    updateCommentDraft();
    el.commentDialog.close();
    toast(`Comment added to #${storyId}`);
    if (state.selectedId === storyId) refreshComments(storyId, { open: true });
  } catch (error) {
    // Keep the text so nothing typed in a meeting is lost.
    el.commentError.textContent = errorText(error);
    el.commentError.classList.remove("hidden");
  } finally {
    el.commentPost.disabled = false;
    el.commentPost.textContent = "Post comment";
  }
}

// ---------- settings ----------

function openSettings() {
  const s = state.settings;
  $("#set-org").value = s.org;
  $("#set-project").value = s.project;
  $("#set-pat").value = "";
  $("#set-pat").placeholder = s.hasPat ? "Saved — leave blank to keep" : "Paste your PAT";
  $("#set-demo").checked = s.demo;
  $("#forget-pat").classList.toggle("hidden", !s.hasPat);
  $("#settings-error").classList.add("hidden");
  $("#settings-cancel").classList.toggle("hidden", !s.configured);
  syncDemoFields();
  el.settings.showModal();
}

function syncDemoFields() {
  const demo = $("#set-demo").checked;
  for (const id of ["#set-org", "#set-project", "#set-pat"]) $(id).disabled = demo;
}

async function saveSettings(extra = {}) {
  const error = $("#settings-error");
  error.classList.add("hidden");
  try {
    state.settings = await invoke("save_settings", {
      input: {
        org: $("#set-org").value,
        project: $("#set-project").value,
        demo: $("#set-demo").checked,
        pat: $("#set-pat").value,
        ...extra,
      },
    });
    if (!state.settings.configured) {
      throw "Fill in the organisation, project and PAT — or tick “Use demo data”.";
    }
    renderBadge();
    el.settings.close();
    state.selectedId = null;
    await loadAll();
  } catch (e) {
    error.textContent = errorText(e);
    error.classList.remove("hidden");
  }
}

function renderBadge() {
  el.demoBadge.classList.toggle("hidden", !state.settings?.demo);
}

// ---------- events ----------

el.team.addEventListener("change", () => changeTeam(el.team.value));
el.sprint.addEventListener("change", () => {
  state.selectedId = null;
  loadStories();
});
el.reload.addEventListener("click", loadAll);
el.openSettings.addEventListener("click", () => openSettings());
el.filter.addEventListener("input", () => {
  state.filter = el.filter.value;
  renderList();
});

el.list.addEventListener("click", (event) => {
  if (event.target.closest('[data-action="settings"]')) return openSettings();
  const move = event.target.closest("[data-move]");
  if (move) return moveSelected(Number(move.dataset.move));
  const item = event.target.closest(".story");
  if (item && Number(item.dataset.id) !== state.selectedId) selectStory(Number(item.dataset.id));
});

el.dock.addEventListener("input", (event) => {
  if (event.target.matches("[data-hours]")) {
    saveDraft();
    refreshDockSummary();
  }
});
el.dock.addEventListener("change", (event) => {
  if (event.target.matches("[data-assignee]")) {
    const kind = event.target.dataset.assignee;
    const skipped = event.target.value === SKIP;
    const row = el.dock.querySelector(`[data-row="${kind}"]`);
    row.classList.toggle("is-skipped", skipped);
    row.querySelector("[data-hours]").disabled = skipped;
    saveDraft();
    writeJson(defaultsKey(), { ...readDefaults(), [kind]: event.target.value });
    refreshDockSummary();
  }
});
el.dock.addEventListener("click", (event) => {
  const again = event.target.closest("[data-again]");
  if (again) {
    const done = readJson(doneKey(state.selectedId), {});
    delete done[again.dataset.again];
    writeJson(doneKey(state.selectedId), done);
    state.dismissed[`${state.selectedId}:${again.dataset.again}`] = true;
    renderDock();
    renderList();
    return;
  }
  if (event.target.closest("#create")) createTasks();
  if (event.target.closest("#open-comment")) openComment();
});

$("#comment-form").addEventListener("submit", (event) => {
  event.preventDefault();
  postComment();
});
el.commentText.addEventListener("input", updateCommentDraft);
$("#comment-cancel").addEventListener("click", () => el.commentDialog.close());

$("#settings-form").addEventListener("submit", (event) => {
  event.preventDefault();
  saveSettings();
});
$("#set-demo").addEventListener("change", syncDemoFields);
$("#settings-cancel").addEventListener("click", () => el.settings.close());
$("#forget-pat").addEventListener("click", async () => {
  await saveSettings({ forgetPat: true, pat: "" });
});

// Open links in the system browser instead of navigating the app window.
document.addEventListener("click", (event) => {
  const link = event.target.closest("a[href]");
  if (!link) return;
  event.preventDefault();
  if (/^https?:/i.test(link.href)) openExternal(link.href);
});

// Click a screenshot to see it full size.
el.detailBody.addEventListener("click", (event) => {
  if (!event.target.matches(".rich img")) return;
  const image = document.createElement("img");
  image.src = event.target.src;
  image.alt = event.target.alt;
  el.lightbox.replaceChildren(image);
  el.lightbox.showModal();
});
el.lightbox.addEventListener("click", () => el.lightbox.close());

document.addEventListener("keydown", (event) => {
  if (event.key === "F5") {
    event.preventDefault();
    loadAll();
  } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && el.commentDialog.open) {
    event.preventDefault();
    postComment();
  } else if (event.key === "Enter" && (event.ctrlKey || event.metaKey) && state.detail && !document.querySelector("dialog[open]")) {
    event.preventDefault();
    createTasks();
  } else if (event.altKey && (event.key === "ArrowDown" || event.key === "ArrowUp") && !event.target.closest("input, select, textarea, dialog")) {
    event.preventDefault();
    moveSelected(event.key === "ArrowDown" ? 1 : -1);
  } else if ((event.key === "ArrowDown" || event.key === "ArrowUp") && !event.target.closest("input, select, textarea, dialog")) {
    const stories = visibleStories();
    const index = stories.findIndex((s) => s.id === state.selectedId);
    const next = stories[index + (event.key === "ArrowDown" ? 1 : -1)];
    if (next) {
      event.preventDefault();
      selectStory(next.id);
    }
  }
});

init();
