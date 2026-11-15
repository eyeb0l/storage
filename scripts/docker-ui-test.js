/* Injected into the native debug Tauri window by test-docker-ui.py. All IPC is real; all Docker
   commands reach an absolute fixture executable in a throwaway HOME. */
(async () => {
  const checks = [];
  const errors = [];
  window.addEventListener("error", (e) => errors.push(e.message));
  window.addEventListener("unhandledrejection", (e) => errors.push(String(e.reason)));
  const assert = (condition, message) => {
    if (!condition) throw new Error(message);
    checks.push(message);
  };
  const wait = async (condition) => {
    const deadline = performance.now() + 15000;
    while (!condition()) {
      if (performance.now() > deadline) throw new Error("Timed out waiting for native UI state");
      await new Promise((resolve) => setTimeout(resolve, 20));
    }
  };
  const rejects = async (fn, message) => {
    let rejected = false;
    try { await fn(); } catch (_) { rejected = true; }
    assert(rejected, message);
  };
  try {
    await wait(() => S.phase === "ready" || S.intro);
    await wait(() => innerWidth === EXPECTED_SIZE[0] && innerHeight === EXPECTED_SIZE[1]);
    assert(innerWidth === EXPECTED_SIZE[0] && innerHeight === EXPECTED_SIZE[1], "Native webview matches the requested window size");
    if (S.intro) await scan();
    assert(S.phase === "ready", "Fake-home filesystem scan completes");
    const cats = JSON.stringify(S.cats);
    const selection = [...S.sel];
    document.querySelector('[data-tab="docker"]').click();
    assert(!$("#dockerView").hidden && $("#cleanView").hidden, "Docker tab displays its own panel");
    assert($("#dockerReview").disabled, "Review starts disabled with no selection");
    $("#dockerLoad").click();
    if (SCENARIO === "cancel") {
      // Wait for the operation's actual backend Busy guard, not an assumed startup delay.
      const deadline = performance.now() + 5000;
      while (true) {
        try { await invoke("docker_prepare_review", {snapshotId:"probe",keys:[]}); }
        catch (error) { if (String(error).includes("Already working")) break; }
        if (performance.now() > deadline) throw new Error("Inventory never acquired backend Busy guard");
        await new Promise((resolve) => setTimeout(resolve, 20));
      }
      $("#dockerStop").click();
    }
    await wait(() => !DockerPanel.busy());
    if (SCENARIO === "cancel") {
      assert($("#dockerStatus").textContent.includes("cancelled"), "Stop cancels an active native Docker collection");
      assert(!$("#dockerContent").children.length && $("#dockerReview").disabled, "Cancelled collection cannot publish partial results");
    } else if (SCENARIO === "failure") {
      assert($("#dockerStatus").textContent.includes("Inventory unavailable"), "CLI failure renders unavailable");
      assert(!$("#dockerContent").children.length && $("#dockerReview").disabled, "Failed collection cannot expose reviewable partial results");
    } else if (SCENARIO === "empty") {
      assert($("#dockerContent").textContent.includes("No containers reported") && $("#dockerReview").disabled, "Empty inventory is distinct from unavailable inventory");
    } else {
      const boxes = [...document.querySelectorAll("input[data-docker-key]")];
      assert(boxes.length === 2, "Container and deduplicated image are individually represented");
      assert($("#dockerContent").textContent.includes("Build cache") && $("#dockerContent").textContent.includes("Unavailable"), "Build-cache limitations are explicit");
      assert(!$("#dockerContent img") && !window.injected, "Docker-provided labels are escaped");
      for (const box of boxes) box.click();
      assert(JSON.stringify([...S.sel]) === JSON.stringify(selection), "Docker review selection never enters cleanup selection");
      assert(JSON.stringify(S.cats) === cats, "Docker object sizes never alter filesystem totals");
      $("#dockerReview").click();
      await wait(() => $("#dockerConfirm").open);
      assert($("#dockerConfirmBody").querySelectorAll(".docker-review-object").length === 2, "Confirmation lists every selected object");
      assert($("#dockerConfirmBody").textContent.includes("a".repeat(64)) && $("#dockerConfirmBody").textContent.includes("sha256:" + "b".repeat(64)), "Confirmation includes both full IDs");
      assert(!$("#dockerConfirmBody").textContent.includes("never-expose-this-secret"), "Private inspect environment never reaches confirmation");
      assert($("#dockerConfirmBody").textContent.includes("/data"), "Container mount destination is visible");
      const actions = $("#dockerCheck").getBoundingClientRect();
      assert(actions.top >= 0 && actions.bottom <= innerHeight, "Review action stays inside the native window");
      const body = $("#dockerConfirmBody");
      if (body.scrollHeight > body.clientHeight + 1) {
        assert(!$("#dockerScrollHint").hidden, "Scrollable review explicitly tells you there are more selected objects");
        body.scrollTop = body.scrollHeight;
        const last = body.lastElementChild.getBoundingClientRect();
        assert(last.bottom <= body.getBoundingClientRect().bottom + 1, "The last reviewed object can be scrolled fully into view");
      }
      assert(document.documentElement.scrollWidth <= innerWidth, "Native window has no horizontal page overflow");
      assert($("#dockerConfirmBody").scrollWidth <= $("#dockerConfirmBody").clientWidth + 1, "Review details wrap long IDs without overflow");
      $("#dockerConfirm").close();
      await wait(() => !DockerPanel.busy());
      $("#dockerReview").click();
      await wait(() => $("#dockerConfirm").open);
      $("#dockerCheck").click();
      await wait(() => !DockerPanel.busy());
      assert($("#dockerStatus").textContent.includes(SCENARIO === "changed" ? "changed since review" : "matched at revalidation"), "Native recheck reports the actual comparison result");
      assert($("#dockerStatus").textContent.includes("Nothing was removed"), "Recheck never claims removal");
      assert($("#dockerReview").disabled && !$("#dockerContent").children.length, "Consumed review requires a fresh inventory");
      if (SCENARIO === "stable") {
        const snapshot = await invoke("docker_inventory");
        const key = snapshot.objects[0].key;
        await rejects(() => invoke("docker_prepare_review", {snapshotId:snapshot.snapshot_id, keys:["unknown"]}), "IPC rejects unknown keys");
        await rejects(() => invoke("docker_prepare_review", {snapshotId:snapshot.snapshot_id, keys:[key,key]}), "IPC rejects duplicate keys");
        const review = await invoke("docker_prepare_review", {snapshotId:snapshot.snapshot_id, keys:[key]});
        await rejects(() => invoke("docker_check_review", {token:"forged"}), "IPC rejects a forged token");
        const result = await invoke("docker_check_review", {token:review.token});
        assert(result.cleanup_enabled === false && result.removed === 0, "Successful IPC review explicitly leaves cleanup disabled");
        await rejects(() => invoke("docker_check_review", {token:review.token}), "IPC rejects token replay");
        const old = await invoke("docker_inventory");
        const stale = await invoke("docker_prepare_review", {snapshotId:old.snapshot_id,keys:[old.objects[0].key]});
        await scan();
        await rejects(() => invoke("docker_check_review", {token:stale.token}), "Filesystem rescan invalidates a prepared token");
        const channel = new TAURI.core.Channel();
        const events = [];
        channel.onmessage = (e) => events.push(e);
        const docker = [...S.entries.values()].find((e) => e.rec === "docker");
        assert(!!docker, "Fake filesystem exposes a Docker.raw entry for safety-boundary proof");
        const refusal = await invoke("clean", {keys:[docker.key],onProgress:channel});
        assert(refusal.failed === 1 && events.some((e) => e.kind === "failed"), "Original Docker cleanup IPC still refuses even after a successful review");
        $("#dockerLoad").click();
        await wait(() => !DockerPanel.busy());
      }
    }
    if (SCENARIO === "stable" && VISUAL_STATE.startsWith("review")) {
      for (const box of document.querySelectorAll("input[data-docker-key]")) box.click();
      $("#dockerReview").click();
      await wait(() => $("#dockerConfirm").open);
      if (VISUAL_STATE === "review-bottom") $("#dockerConfirmBody").scrollTop = $("#dockerConfirmBody").scrollHeight;
    }
    if (VISUAL_STATE === "details") $("#dockerContent details").open = true;
    $("#dockerView").scrollIntoView({block:"start"});
    if (VISUAL_STATE === "inventory-bottom") $(".docker-foot").scrollIntoView({block:"end"});
    assert(document.documentElement.scrollWidth <= innerWidth, "Final panel has no horizontal overflow at the requested size");
    if (CAPTURE) {
      // Finish transient CSS fades before capture; no arbitrary settling delay or visual overrides.
      for (const animation of document.getAnimations()) {
        try { animation.finish(); } catch (_) { animation.cancel(); }
      }
      assert(getComputedStyle($("#dockerView")).opacity === "1", "Native snapshot captures the settled panel state");
      await invoke("dev_snapshot");
    }
    assert(errors.length === 0, "No native webview errors or unhandled rejections");
    await invoke("dev_report", {text:JSON.stringify({status:"passed",scenario:SCENARIO,checks,errors,viewport:{width:innerWidth,height:innerHeight},title:document.title})});
  } catch (error) {
    await invoke("dev_report", {text:JSON.stringify({status:"failed",scenario:SCENARIO,checks,errors,error:String(error),stack:error.stack,body:document.body.innerText})});
  }
})();
