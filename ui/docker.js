"use strict";

/* A separate, memory-only review selection. Never passes anything to the cleanup command. */
const DockerPanel = (() => {
  const D = { snapshot: null, selected: new Set(), pending: null, busy: false, request: 0, message: "", timer: null };
  const objects = () => D.snapshot?.objects || [];
  const size = (o) => o.bytes == null ? "Size unknown" : `${fmt(o.bytes)} ${o.kind === "container" ? "writable layer" : "logical image size"}`;

  function reviewLayout() {
    if (!$("#dockerConfirm").open) return;
    const body = $("#dockerConfirmBody");
    const hint = $("#dockerScrollHint");
    hint.hidden = true;
    hint.hidden = body.scrollHeight <= body.clientHeight + 1;
  }

  function details(o) {
    const references = o.references.length ? `<dt>References</dt><dd>${o.references.map(esc).join("<br>")}</dd>` : "<dt>References</dt><dd>None reported</dd>";
    const mounts = o.mounts?.length ? o.mounts.map((m) => `${esc(m.kind)}${m.name ? ` · ${esc(m.name)}` : ""} → ${esc(m.destination)}`).join("<br>") : "None reported";
    return `<dl class="docker-details"><dt>Full ID</dt><dd class="docker-id">${esc(o.id)}</dd>
      <dt>Created</dt><dd>${esc(o.created)}</dd>${o.image ? `<dt>Image</dt><dd class="docker-id">${esc(o.image)}</dd>` : ""}
      ${o.project ? `<dt>Compose project</dt><dd>${esc(o.project)}</dd>` : ""}${references}
      ${o.kind === "container" ? `<dt>Mounts</dt><dd>${mounts}</dd>` : ""}
      <dt>Cleanup</dt><dd>Disabled: ${esc(o.hold)}.</dd></dl>`;
  }

  function discard() {
    clearTimeout(D.timer);
    const review = D.pending;
    D.pending = null;
    if (review) invoke("docker_discard_review", { token: review.token }).catch(() => {});
  }

  function invalidate(message) {
    const hadReview = !!(D.snapshot || D.pending || D.selected.size);
    ++D.request;
    discard();
    if ($("#dockerConfirm").open) $("#dockerConfirm").close();
    D.snapshot = null;
    D.selected.clear();
    D.message = message ?? (hadReview ? "The previous review was cleared. Load a fresh Docker inventory." : "Load an inventory from Docker Desktop to inspect its objects.");
    renderInventory();
    render();
  }

  function renderSelection() {
    $("#dockerSelection").textContent = D.selected.size ? `${plural(D.selected.size, "object")} selected for review only` : "No objects selected for review";
    $("#dockerReview").disabled = isBusy() || !D.selected.size;
  }

  function render() {
    $("#dockerLoad").disabled = isBusy();
    $("#dockerLoad").textContent = D.snapshot ? "Refresh inventory" : "Load inventory";
    $("#dockerStop").hidden = !D.busy;
    $("#dockerView").setAttribute("aria-busy", String(D.busy));
    $("#dockerStatus").textContent = D.message || "Load an inventory from Docker Desktop to inspect its objects.";
    for (const checkbox of document.querySelectorAll("input[data-docker-key]")) checkbox.disabled = isBusy();
    renderSelection();
  }

  function renderInventory() {
    if (!D.snapshot) {
      $("#dockerContent").replaceChildren();
      return;
    }
    const snapshot = D.snapshot;
    const groups = ["container", "image"].map((kind) => {
      const rows = objects().filter((o) => o.kind === kind);
      return `<section class="docker-group" aria-label="${kind === "container" ? "Containers" : "Images"}">
        <h3>${kind === "container" ? "Containers" : "Images"} <span class="num muted">${rows.length}</span></h3>
        ${rows.length ? rows.map((o) => `<article class="docker-object">
          <div class="docker-object-head"><label><input type="checkbox" data-docker-key="${esc(o.key)}" aria-label="Select ${esc(o.label)} for review only">
            <span>${esc(o.label)}</span></label><span class="docker-size num">${esc(size(o))}</span></div>
          <p class="docker-object-state">${esc(o.state)} · Cleanup disabled${o.kind === "container" ? " · Writable data may be irreplaceable" : " · Layers may be shared"}</p>
          <details><summary>Object details <span class="docker-short-id">${esc(o.id.slice(0, 19))}…</span></summary>${details(o)}</details>
        </article>`).join("") : `<p class="docker-note">No ${kind === "container" ? "containers" : "images"} reported.</p>`}</section>`;
    }).join("");
    $("#dockerContent").innerHTML = `<p class="docker-engine">${esc(snapshot.engine.context)} · Engine <span class="docker-id">${esc(snapshot.engine.id)}</span><br>
      Collected ${esc(new Date(snapshot.collected_at * 1000).toLocaleString())}</p>
      <p class="docker-note">Docker sizes include shared data and do not predict recovered disk space. They are already inside the disk image counted in the overview.</p>
      ${groups}<section class="docker-group"><h3>Build cache <span class="pill review">Unavailable</span></h3>
      <p class="docker-note">Storage cannot yet verify individual build-cache objects and their builder. Build-cache review and cleanup are unavailable.</p></section>`;
  }

  async function load() {
    if (isBusy()) return;
    invalidate("");
    const request = D.request;
    D.busy = true;
    D.message = "Reading Docker Desktop…";
    renderAll();
    try {
      const snapshot = await invoke("docker_inventory");
      if (request !== D.request) return;
      D.snapshot = snapshot;
      D.message = `${plural(snapshot.objects.length, "object")} inventoried. Select objects to review; nothing can be removed.`;
    } catch (error) {
      if (request === D.request) D.message = `Inventory unavailable: ${String(error)}. Nothing was removed.`;
    } finally {
      D.busy = false;
      renderInventory();
      renderAll();
    }
  }

  async function prepare() {
    if (isBusy() || !D.snapshot || !D.selected.size) return;
    D.busy = true;
    const request = D.request;
    renderAll();
    try {
      const review = await invoke("docker_prepare_review", { snapshotId: D.snapshot.snapshot_id, keys: [...D.selected] });
      if (request !== D.request) {
        invoke("docker_discard_review", { token: review.token }).catch(() => {});
        return;
      }
      D.pending = review;
      $("#dockerConfirmBody").innerHTML = review.objects.map((o) => `<section class="docker-review-object"><h3>${esc(o.label)}</h3>
        <p>${esc(o.kind)} · ${esc(o.state)} · ${esc(size(o))}</p>${details(o)}</section>`).join("");
      $("#dockerReviewExpiry").textContent = `Reviewing ${plural(review.objects.length, "object")}. This review expires in at most five minutes.`;
      $("#dockerCheck").disabled = false;
      $("#dockerConfirm").showModal();
      $("#dockerConfirmBody").scrollTop = 0;
      reviewLayout();
      D.timer = setTimeout(() => {
        discard();
        $("#dockerCheck").disabled = true;
        $("#dockerReviewExpiry").textContent = "Review expired. Close this dialog and review again.";
      }, Math.max(0, review.expires_at * 1000 - Date.now()));
    } catch (error) {
      D.message = `Review unavailable: ${String(error)}. Load a fresh inventory.`;
      D.snapshot = null;
      D.selected.clear();
      renderInventory();
    } finally {
      D.busy = false;
      renderAll();
    }
  }

  async function check() {
    const review = D.pending;
    if (!review || isBusy()) return;
    // The backend consumes this token before reinspection. Never retain it for a retry.
    clearTimeout(D.timer);
    D.pending = null;
    $("#dockerConfirm").close();
    D.snapshot = null;
    D.selected.clear();
    D.busy = true;
    D.message = "Rechecking the reviewed objects…";
    renderInventory();
    renderAll();
    try {
      const result = await invoke("docker_check_review", { token: review.token });
      D.message = result.message;
    } catch (error) {
      D.message = `Review could not be validated: ${String(error)}. Nothing was removed. Load a fresh inventory.`;
    } finally {
      D.busy = false;
      renderAll();
      $("#dockerLoad").focus();
    }
  }

  function init() {
    $("#dockerLoad").addEventListener("click", load);
    $("#dockerStop").addEventListener("click", () => invoke("cancel").catch(() => {}));
    $("#dockerReview").addEventListener("click", prepare);
    $("#dockerCheck").addEventListener("click", check);
    $("#dockerConfirm").addEventListener("close", discard);
    window.addEventListener("resize", reviewLayout);
    $("#dockerContent").addEventListener("change", (event) => {
      const key = event.target.dataset.dockerKey;
      if (!key || isBusy()) return;
      discard();
      event.target.checked ? D.selected.add(key) : D.selected.delete(key);
      renderSelection();
    });
    render();
  }

  return { init, render, invalidate, load, busy: () => D.busy };
})();
