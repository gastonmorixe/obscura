// chrome / browser API shim for Obscura.
//
// Scope: just enough for real-world MV2 content-script extensions to
// bring up without crashing on initial load. APIs that extensions
// touch but Obscura has no meaningful behaviour for (badges, tab UI,
// option pages) are graceful no-ops. APIs that extensions typically
// depend on for correctness (storage, runtime messaging, executeScript,
// getManifest) have real impls.
//
// This file is `include_str!`-baked into the obscura-ext crate; it runs
// inside an IIFE that has these globals pre-bound by the host:
//   __obscura_ext_bundle      // {relative_path: source_text} for all .js
//   __obscura_ext_manifest    // serialised manifest
//   __obscura_ext_id          // extension id string
//   __obscura_ext_url         // current page url
//   __obscura_ext_mv          // manifest_version (2 or 3)

(function () {
  "use strict";

  // ---- tiny in-realm storage backing chrome.storage.local ---------------
  // Persistence across navigations would belong on the Rust side; for our
  // single-navigation fetches, the extension's reads and writes both
  // happen within one preload, so per-page memory is sufficient. For
  // long-running `obscura serve` sessions, this lifts to
  // `ExtensionState` (Rust side, already has a placeholder).
  const _storageAreas = {};
  function _mkStorageArea() {
    const data = Object.create(null);
    return {
      _data: data,
      get(keys, cb) {
        // Match the real Chrome API surface, but with SYNCHRONOUS callback
        // dispatch. Real Chrome posts to a different process; here storage
        // is in-memory in the same realm, and a common extension
        // bootstrap pattern is "storage.local.get(defaults, cb) → in cb,
        // populate an `enabledSites` array → arm tabs.onUpdated". If we
        // defer the cb via a microtask, the synthesised onUpdated fires
        // first against an empty enabledSites and every site is skipped.
        let result;
        if (keys === null || keys === undefined) {
          result = Object.assign({}, data);
        } else if (typeof keys === "string") {
          result = data[keys] === undefined ? {} : { [keys]: data[keys] };
        } else if (Array.isArray(keys)) {
          result = {};
          for (const k of keys) result[k] = data[k] !== undefined ? data[k] : undefined;
          // Drop undefined entries to match real Chrome semantics.
          for (const k of keys) if (result[k] === undefined) delete result[k];
        } else if (typeof keys === "object") {
          // `keys` is a defaults map: caller-supplied defaults seen when
          // storage doesn't have the entry.
          result = Object.assign({}, keys);
          for (const k of Object.keys(keys)) if (data[k] !== undefined) result[k] = data[k];
        } else {
          result = {};
        }
        if (typeof cb === "function") {
          try { cb(result); } catch (e) { console.error("storage.local.get cb threw:", e && e.stack || e); }
        }
        return Promise.resolve(result);
      },
      set(items, cb) {
        Object.assign(data, items || {});
        if (typeof cb === "function") { try { cb(); } catch (e) { console.error("storage.set cb threw:", e); } }
        return Promise.resolve();
      },
      remove(keys, cb) {
        const ks = Array.isArray(keys) ? keys : [keys];
        for (const k of ks) delete data[k];
        if (typeof cb === "function") { try { cb(); } catch (e) { console.error("storage.remove cb threw:", e); } }
        return Promise.resolve();
      },
      clear(cb) {
        for (const k of Object.keys(data)) delete data[k];
        if (typeof cb === "function") { try { cb(); } catch (e) { console.error("storage.clear cb threw:", e); } }
        return Promise.resolve();
      },
      getBytesInUse(_keys, cb) {
        const bytes = JSON.stringify(data).length;
        if (typeof cb === "function") { try { cb(bytes); } catch (e) { console.error("storage.getBytesInUse cb threw:", e); } }
        return Promise.resolve(bytes);
      },
      onChanged: { addListener() {}, removeListener() {}, hasListener() { return false; } },
    };
  }
  _storageAreas.local = _mkStorageArea();
  _storageAreas.sync = _mkStorageArea();
  _storageAreas.session = _mkStorageArea();
  _storageAreas.managed = _mkStorageArea();

  // ---- runtime.onMessage / tabs.sendMessage event bus -------------------
  // Same-realm dispatch: bg and content scripts share the global, so when
  // bg calls `tabs.sendMessage(tabId, msg)`, we synchronously invoke every
  // registered `runtime.onMessage` listener.
  const _onMessageListeners = [];
  function _dispatchMessage(message, sender) {
    let responded = false;
    let response = undefined;
    const sendResponse = (resp) => {
      responded = true;
      response = resp;
    };
    for (const fn of _onMessageListeners) {
      try {
        const ret = fn(message, sender || { id: __obscura_ext_id, tab: _currentTab() }, sendResponse);
        if (ret === true) {
          // Listener will call sendResponse asynchronously; we don't
          // wait for it in this synchronous shim.
        }
      } catch (e) {
        console.error("obscura-ext: onMessage listener threw:", e && e.stack || e);
      }
    }
    return responded ? response : undefined;
  }

  // ---- "tabs" model: Obscura has one Page per BrowserContext, so we
  //      synthesise a single tab. tabId is constant within a page lifetime.
  const TAB_ID = 1;
  function _currentTab() {
    return {
      id: TAB_ID,
      url: __obscura_ext_url,
      title: (globalThis.document && globalThis.document.title) || "",
      active: true,
      windowId: 1,
      index: 0,
      pinned: false,
      incognito: false,
      status: "complete",
      highlighted: true,
      audible: false,
      mutedInfo: { muted: false },
      discarded: false,
      autoDiscardable: true,
      groupId: -1,
      // Most extensions don't read these but real Chrome
      // surfaces them — keep callers from null-deref'ing.
    };
  }

  // ---- onUpdated firing ------------------------------------------------
  // A canonical extension idiom is:
  //   chrome.tabs.onUpdated.addListener(function(tabId, changeInfo, tab) {
  //     if (changeInfo.status === 'complete') { ... per-tab work ... }
  //   });
  // We capture those listeners and fire them once with status:'complete'
  // for the current tab, right after background.js finishes evaluating.
  const _onUpdatedListeners = [];

  // ---- executeScript dispatch: looks up files in the registry and
  //      evals them in the same realm. The cb gets called with an array
  //      of per-frame results (real Chrome semantics).
  function _executeFile(file) {
    const src = globalThis.__obscura_ext_bundle[file];
    if (typeof src !== "string") {
      console.warn("obscura-ext: requested script not in bundle: " + file);
      throw new Error("obscura-ext: requested script not in bundle: " + file);
    }
    // Indirect eval so the script runs at global scope. Real-world
    // content scripts and the helper files they load typically define
    // top-level functions and module-scoped state expected to be
    // visible to each other; running them via direct `eval` would scope
    // all of that to the current closure and break the inter-script
    // wire-up.
    try {
      (0, eval)(src);
    } catch (e) {
      console.error("obscura-ext: executeScript failed for " + file + ":", e && e.stack || e);
      throw e;
    }
  }

  function _tabsExecuteScript(arg1, arg2, arg3) {
    // Real Chrome MV2 signatures:
    //   chrome.tabs.executeScript(details, callback)
    //   chrome.tabs.executeScript(tabId, details, callback)
    //
    // CRITICAL: callbacks fire SYNCHRONOUSLY here, not via microtask.
    // A common extension idiom is a deep nested executeScript chain
    // (load helper.js → cs.js → cs_locale.js via callback chaining) and
    // then a delayed `tabs.sendMessage` carrying per-page config. With
    // Obscura's synchronous-drain timer model, microtask-deferred
    // callbacks would leave the cs (which registers the onMessage
    // listener) unloaded at the moment that config message fires — the
    // listener doesn't exist yet and the per-site behaviour never runs.
    // Synchronous callbacks pin the load order correctly.
    let details, callback;
    if (typeof arg1 === "number") {
      details = arg2;
      callback = arg3;
    } else {
      details = arg1;
      callback = arg2;
    }
    details = details || {};
    if (details.file) {
      try {
        _executeFile(details.file);
        if (typeof callback === "function") {
          try { callback([undefined]); } catch (e) { console.error("executeScript cb threw:", e && e.stack || e); }
        }
        return Promise.resolve([undefined]);
      } catch (e) {
        if (typeof callback === "function") { try { callback(); } catch {} }
        return Promise.reject(e);
      }
    }
    if (typeof details.code === "string") {
      try {
        (0, eval)(details.code);
        if (typeof callback === "function") { try { callback([undefined]); } catch {} }
        return Promise.resolve([undefined]);
      } catch (e) {
        if (typeof callback === "function") { try { callback(); } catch {} }
        return Promise.reject(e);
      }
    }
    if (typeof callback === "function") { try { callback([]); } catch {} }
    return Promise.resolve([]);
  }

  // MV3 form: scripting.executeScript({target, files, world}).
  function _scriptingExecuteScript(injection) {
    injection = injection || {};
    const files = injection.files || [];
    const out = [];
    for (const f of files) {
      try {
        _executeFile(f);
        out.push({ frameId: 0, result: undefined });
      } catch (e) {
        out.push({ frameId: 0, error: String(e) });
      }
    }
    return Promise.resolve(out);
  }

  // ---- webRequest: record listeners so the host can run the
  // request-header rewrites (blocking `onBeforeSendHeaders`) that MV2
  // paywall extensions use to set Referer / User-Agent on the top-level
  // document. The host invokes `__obscura_ext_collect_request_headers`
  // BEFORE fetching the document, applies the result to the outbound
  // request, and the page comes back unlocked. Listeners for other events
  // are still recorded (harmlessly unused) so bg code paths don't break.
  globalThis.__obscura_ext_wr_listeners = globalThis.__obscura_ext_wr_listeners || {};
  function _mkWREvent(name) {
    const reg = globalThis.__obscura_ext_wr_listeners;
    reg[name] = reg[name] || [];
    return {
      addListener(fn /*, filter, extraInfoSpec */) {
        if (typeof fn === "function") reg[name].push(fn);
      },
      removeListener(fn) {
        const arr = reg[name];
        if (!arr) return;
        const i = arr.indexOf(fn);
        if (i >= 0) arr.splice(i, 1);
      },
      hasListener(fn) { return (reg[name] || []).includes(fn); },
      hasListeners() { return (reg[name] || []).length > 0; },
      __name: name,
    };
  }

  // Run all registered `onBeforeSendHeaders` listeners against a synthetic
  // request for `url` (resourceType `main_frame`), threading each listener's
  // `{requestHeaders}` return into the next, exactly as Chrome does for
  // blocking listeners. `baseHeaders` is an array of {name,value} the host
  // will actually send (so the extension can rewrite an existing Referer /
  // User-Agent in place). Returns the final header array as a {name:value}
  // object (last write wins). Safe to call with no listeners — returns {}.
  globalThis.__obscura_ext_collect_request_headers = function (url, baseHeaders) {
    const reg = globalThis.__obscura_ext_wr_listeners || {};
    const listeners = reg["onBeforeSendHeaders"] || [];
    let headers = Array.isArray(baseHeaders) ? baseHeaders.map(h => ({ name: h.name, value: h.value })) : [];
    const details = {
      url: url,
      method: "GET",
      type: "main_frame",
      tabId: (globalThis.__obscura_ext_tab_id || 1),
      frameId: 0,
      requestId: String(Date.now()),
      timeStamp: Date.now(),
      requestHeaders: headers,
    };
    for (const fn of listeners) {
      try {
        const ret = fn(details);
        if (ret && Array.isArray(ret.requestHeaders)) {
          details.requestHeaders = ret.requestHeaders;
        }
      } catch (e) {
        console.error("obscura-ext: onBeforeSendHeaders listener threw:", e && e.stack || e);
      }
    }
    const out = {};
    for (const h of details.requestHeaders) {
      if (h && typeof h.name === "string") out[h.name] = h.value;
    }
    return out;
  };

  // ---- declarativeNetRequest -------------------------------------------
  // Real Chrome applies these rules inside the network stack. Obscura's
  // network stack is in Rust, so the shim's job is to faithfully maintain
  // the session/dynamic rule set in a realm global
  // (`__obscura_ext_dnr_rules`) that the host harvests AFTER the background
  // scripts run and BEFORE the top-level document is (re)fetched. The host
  // only acts on `modifyHeaders` request-header rules today (see
  // `obscura_ext::dnr`); other rule types are stored verbatim so
  // `getSessionRules()` round-trips correctly for extensions that read
  // their own rules back.
  globalThis.__obscura_ext_dnr_rules = globalThis.__obscura_ext_dnr_rules || {
    session: {},  // id -> rule
    dynamic: {},  // id -> rule
  };
  function _mkDNR() {
    const store = globalThis.__obscura_ext_dnr_rules;
    function _update(bucket, opts) {
      opts = opts || {};
      const removeIds = opts.removeRuleIds || [];
      for (const id of removeIds) delete bucket[id];
      const addRules = opts.addRules || [];
      for (const rule of addRules) {
        if (rule && rule.id != null) bucket[rule.id] = rule;
      }
    }
    return {
      updateSessionRules(opts, cb) {
        try { _update(store.session, opts); } catch (e) { console.error("updateSessionRules:", e); }
        if (typeof cb === "function") Promise.resolve().then(cb);
        return Promise.resolve();
      },
      updateDynamicRules(opts, cb) {
        try { _update(store.dynamic, opts); } catch (e) { console.error("updateDynamicRules:", e); }
        if (typeof cb === "function") Promise.resolve().then(cb);
        return Promise.resolve();
      },
      getSessionRules(cb) {
        const rules = Object.values(store.session);
        if (typeof cb === "function") Promise.resolve().then(() => cb(rules));
        return Promise.resolve(rules);
      },
      getDynamicRules(cb) {
        const rules = Object.values(store.dynamic);
        if (typeof cb === "function") Promise.resolve().then(() => cb(rules));
        return Promise.resolve(rules);
      },
      getEnabledRulesets(cb) {
        if (typeof cb === "function") Promise.resolve().then(() => cb([]));
        return Promise.resolve([]);
      },
      updateEnabledRulesets(_opts, cb) {
        if (typeof cb === "function") Promise.resolve().then(cb);
        return Promise.resolve();
      },
      getAvailableStaticRuleCount(cb) {
        if (typeof cb === "function") Promise.resolve().then(() => cb(30000));
        return Promise.resolve(30000);
      },
      setExtensionActionOptions(_opts, cb) {
        if (typeof cb === "function") Promise.resolve().then(cb);
        return Promise.resolve();
      },
      isRegexSupported(_opts, cb) {
        const r = { isSupported: true };
        if (typeof cb === "function") Promise.resolve().then(() => cb(r));
        return Promise.resolve(r);
      },
      onRuleMatchedDebug: { addListener() {}, removeListener() {}, hasListener() { return false; } },
      MAX_NUMBER_OF_DYNAMIC_AND_SESSION_RULES: 30000,
      MAX_NUMBER_OF_SESSION_RULES: 5000,
      MAX_NUMBER_OF_DYNAMIC_RULES: 30000,
      MAX_NUMBER_OF_REGEX_RULES: 1000,
      DYNAMIC_RULESET_ID: "_dynamic",
      SESSION_RULESET_ID: "_session",
    };
  }

  // ---- cookies: extensions read cookies post-load for things like
  // scrubbing per-site state counters. Stubbed to "empty" semantics.
  // Wiring this to the real `obscura-net::CookieJar` is straightforward
  // but not done — see docs/extensions.md.
  function _cookiesGetAll(query, cb) {
    if (typeof cb === "function") Promise.resolve().then(() => cb([]));
    return Promise.resolve([]);
  }
  function _cookiesGet(query, cb) {
    if (typeof cb === "function") Promise.resolve().then(() => cb(null));
    return Promise.resolve(null);
  }
  function _cookiesSet(details, cb) {
    if (typeof cb === "function") Promise.resolve().then(() => cb(details));
    return Promise.resolve(details);
  }
  function _cookiesRemove(details, cb) {
    if (typeof cb === "function") Promise.resolve().then(() => cb(details));
    return Promise.resolve(details);
  }

  // ---- the global itself ------------------------------------------------
  const chrome = {
    runtime: {
      id: __obscura_ext_id,
      lastError: null,
      OnInstalledReason: { INSTALL: "install", UPDATE: "update" },
      getManifest() { return __obscura_ext_manifest; },
      getURL(rel) {
        const r = String(rel || "").replace(/^\/+/, "");
        return "chrome-extension://" + __obscura_ext_id + "/" + r;
      },
      getPlatformInfo(cb) {
        const info = { os: "mac", arch: "arm64", nacl_arch: "arm" };
        if (typeof cb === "function") Promise.resolve().then(() => cb(info));
        return Promise.resolve(info);
      },
      getBrowserInfo(cb) {
        const info = { name: "Obscura", vendor: "Obscura", version: "0.1.0", buildID: "0" };
        if (typeof cb === "function") Promise.resolve().then(() => cb(info));
        return Promise.resolve(info);
      },
      onInstalled: { addListener() {}, removeListener() {}, hasListener() { return false; } },
      onStartup: { addListener() {}, removeListener() {}, hasListener() { return false; } },
      onMessage: {
        addListener(fn) { if (typeof fn === "function") _onMessageListeners.push(fn); },
        removeListener(fn) {
          const i = _onMessageListeners.indexOf(fn);
          if (i >= 0) _onMessageListeners.splice(i, 1);
        },
        hasListener(fn) { return _onMessageListeners.indexOf(fn) >= 0; },
      },
      onConnect: { addListener() {}, removeListener() {} },
      onConnectExternal: { addListener() {}, removeListener() {} },
      onMessageExternal: { addListener() {}, removeListener() {} },
      onUpdateAvailable: { addListener() {}, removeListener() {} },
      sendMessage(extensionIdOrMessage, message, options, callback) {
        // Many real-world callers pass `(message, cb)` or just `(message)`.
        let msg, cb;
        if (typeof extensionIdOrMessage === "string" && message !== undefined) {
          msg = message;
          cb = typeof options === "function" ? options : callback;
        } else {
          msg = extensionIdOrMessage;
          cb = typeof message === "function" ? message : (typeof options === "function" ? options : callback);
        }
        const resp = _dispatchMessage(msg, { id: __obscura_ext_id });
        if (typeof cb === "function") Promise.resolve().then(() => cb(resp));
        return Promise.resolve(resp);
      },
      connect() {
        return {
          name: "", postMessage() {}, disconnect() {},
          onMessage: { addListener() {}, removeListener() {} },
          onDisconnect: { addListener() {}, removeListener() {} },
        };
      },
      openOptionsPage(cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
      reload() {},
      setUninstallURL(_url, cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
    },
    storage: {
      local: _storageAreas.local,
      sync: _storageAreas.sync,
      session: _storageAreas.session,
      managed: _storageAreas.managed,
      onChanged: { addListener() {}, removeListener() {}, hasListener() { return false; } },
    },
    tabs: {
      query(query, cb) {
        const tabs = [_currentTab()];
        if (typeof cb === "function") Promise.resolve().then(() => cb(tabs));
        return Promise.resolve(tabs);
      },
      get(tabId, cb) {
        const tab = _currentTab();
        if (typeof cb === "function") Promise.resolve().then(() => cb(tab));
        return Promise.resolve(tab);
      },
      getCurrent(cb) {
        const tab = _currentTab();
        if (typeof cb === "function") Promise.resolve().then(() => cb(tab));
        return Promise.resolve(tab);
      },
      sendMessage(tabId, msg, _options, cb) {
        if (typeof _options === "function" && cb === undefined) { cb = _options; }
        const resp = _dispatchMessage(msg, { tab: _currentTab(), id: __obscura_ext_id });
        if (typeof cb === "function") Promise.resolve().then(() => cb(resp));
        return Promise.resolve(resp);
      },
      executeScript: _tabsExecuteScript,
      insertCSS(_a, _b, cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
      removeCSS(_a, _b, cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
      reload(_a, _b, cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
      update(_a, _b, cb) {
        const tab = _currentTab();
        if (typeof cb === "function") Promise.resolve().then(() => cb(tab));
        return Promise.resolve(tab);
      },
      create(_a, cb) {
        const tab = _currentTab();
        if (typeof cb === "function") Promise.resolve().then(() => cb(tab));
        return Promise.resolve(tab);
      },
      remove(_a, cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
      onUpdated: {
        addListener(fn) { if (typeof fn === "function") _onUpdatedListeners.push(fn); },
        removeListener(fn) {
          const i = _onUpdatedListeners.indexOf(fn);
          if (i >= 0) _onUpdatedListeners.splice(i, 1);
        },
        hasListener(fn) { return _onUpdatedListeners.indexOf(fn) >= 0; },
      },
      onActivated: { addListener() {}, removeListener() {} },
      onCreated: { addListener() {}, removeListener() {} },
      onRemoved: { addListener() {}, removeListener() {} },
      onAttached: { addListener() {}, removeListener() {} },
      onDetached: { addListener() {}, removeListener() {} },
      onReplaced: { addListener() {}, removeListener() {} },
      onZoomChange: { addListener() {}, removeListener() {} },
      onMoved: { addListener() {}, removeListener() {} },
      onHighlighted: { addListener() {}, removeListener() {} },
      TAB_ID_NONE: -1,
    },
    scripting: {
      executeScript: _scriptingExecuteScript,
      insertCSS() { return Promise.resolve(); },
      removeCSS() { return Promise.resolve(); },
      registerContentScripts() { return Promise.resolve(); },
      unregisterContentScripts() { return Promise.resolve(); },
      getRegisteredContentScripts() { return Promise.resolve([]); },
      updateContentScripts() { return Promise.resolve(); },
    },
    permissions: {
      contains(_perms, cb) {
        // Extensions typically check `permissions.contains({origins: [...]})`
        // before running their per-site logic. We auto-grant everything
        // in the manifest since the user already accepted by passing
        // --extension on the CLI.
        if (typeof cb === "function") Promise.resolve().then(() => cb(true));
        return Promise.resolve(true);
      },
      request(_perms, cb) {
        if (typeof cb === "function") Promise.resolve().then(() => cb(true));
        return Promise.resolve(true);
      },
      remove(_perms, cb) {
        if (typeof cb === "function") Promise.resolve().then(() => cb(true));
        return Promise.resolve(true);
      },
      getAll(cb) {
        const all = { permissions: [], origins: [] };
        if (typeof cb === "function") Promise.resolve().then(() => cb(all));
        return Promise.resolve(all);
      },
      onAdded: { addListener() {}, removeListener() {} },
      onRemoved: { addListener() {}, removeListener() {} },
    },
    cookies: {
      get: _cookiesGet,
      getAll: _cookiesGetAll,
      set: _cookiesSet,
      remove: _cookiesRemove,
      getAllCookieStores(cb) {
        const stores = [{ id: "0", tabIds: [TAB_ID] }];
        if (typeof cb === "function") Promise.resolve().then(() => cb(stores));
        return Promise.resolve(stores);
      },
      onChanged: { addListener() {}, removeListener() {} },
    },
    webRequest: {
      onBeforeRequest: _mkWREvent("onBeforeRequest"),
      onBeforeSendHeaders: _mkWREvent("onBeforeSendHeaders"),
      onSendHeaders: _mkWREvent("onSendHeaders"),
      onHeadersReceived: _mkWREvent("onHeadersReceived"),
      onAuthRequired: _mkWREvent("onAuthRequired"),
      onResponseStarted: _mkWREvent("onResponseStarted"),
      onBeforeRedirect: _mkWREvent("onBeforeRedirect"),
      onCompleted: _mkWREvent("onCompleted"),
      onErrorOccurred: _mkWREvent("onErrorOccurred"),
      // Extensions check .hasOwnProperty('EXTRA_HEADERS') to decide
      // whether to request the `extraHeaders` extraInfoSpec when
      // registering listeners. We do support it (we're the network
      // stack), so advertise it.
      OnBeforeSendHeadersOptions: { EXTRA_HEADERS: "extraHeaders", REQUEST_BODY: "requestBody", BLOCKING: "blocking", ASYNC_BLOCKING: "asyncBlocking" },
      OnBeforeRequestOptions: { EXTRA_HEADERS: "extraHeaders", BLOCKING: "blocking", REQUEST_BODY: "requestBody" },
      OnHeadersReceivedOptions: { EXTRA_HEADERS: "extraHeaders", BLOCKING: "blocking", RESPONSE_HEADERS: "responseHeaders" },
      handlerBehaviorChanged(cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
    },
    declarativeNetRequest: _mkDNR(),
    action: {
      setBadgeText() {},
      setBadgeBackgroundColor() {},
      setBadgeTextColor() {},
      setIcon() {},
      setTitle() {},
      setPopup() {},
      getBadgeText(_d, cb) { if (typeof cb === "function") Promise.resolve().then(() => cb("")); return Promise.resolve(""); },
      enable() {},
      disable() {},
      onClicked: { addListener() {}, removeListener() {} },
    },
    management: {
      getSelf(cb) {
        const self = {
          id: __obscura_ext_id,
          name: __obscura_ext_manifest.name,
          version: __obscura_ext_manifest.version,
          shortName: __obscura_ext_manifest.name,
          description: "",
          enabled: true,
          installType: "development",
          mayDisable: true,
          mayEnable: true,
          type: "extension",
          hostPermissions: [],
          permissions: __obscura_ext_manifest.permissions || [],
        };
        if (typeof cb === "function") Promise.resolve().then(() => cb(self));
        return Promise.resolve(self);
      },
      get(_id, cb) {
        const self = { id: __obscura_ext_id, enabled: true };
        if (typeof cb === "function") Promise.resolve().then(() => cb(self));
        return Promise.resolve(self);
      },
    },
    extension: {
      inIncognitoContext: false,
      getURL(rel) {
        const r = String(rel || "").replace(/^\/+/, "");
        return "chrome-extension://" + __obscura_ext_id + "/" + r;
      },
      getBackgroundPage() { return globalThis; },
      isAllowedFileSchemeAccess(cb) { if (typeof cb === "function") Promise.resolve().then(() => cb(false)); return Promise.resolve(false); },
      isAllowedIncognitoAccess(cb) { if (typeof cb === "function") Promise.resolve().then(() => cb(false)); return Promise.resolve(false); },
    },
    windows: {
      getCurrent(cb) {
        const w = { id: 1, focused: true, top: 0, left: 0, width: 1280, height: 800, tabs: [_currentTab()], type: "normal", state: "normal", alwaysOnTop: false, incognito: false };
        if (typeof cb === "function") Promise.resolve().then(() => cb(w));
        return Promise.resolve(w);
      },
      getAll(_o, cb) {
        const ws = [{ id: 1, focused: true, type: "normal", tabs: [_currentTab()] }];
        if (typeof cb === "function") Promise.resolve().then(() => cb(ws));
        return Promise.resolve(ws);
      },
      onFocusChanged: { addListener() {}, removeListener() {} },
      onCreated: { addListener() {}, removeListener() {} },
      onRemoved: { addListener() {}, removeListener() {} },
      WINDOW_ID_NONE: -1,
      WINDOW_ID_CURRENT: -2,
    },
    notifications: {
      create(_id, _opts, cb) { if (typeof cb === "function") Promise.resolve().then(() => cb("0")); return Promise.resolve("0"); },
      clear(_id, cb) { if (typeof cb === "function") Promise.resolve().then(() => cb(true)); return Promise.resolve(true); },
      onClicked: { addListener() {}, removeListener() {} },
      onClosed: { addListener() {}, removeListener() {} },
    },
    i18n: {
      getMessage(name, _subs) { return ""; },
      getUILanguage() { return "en-US"; },
      getAcceptLanguages(cb) { if (typeof cb === "function") Promise.resolve().then(() => cb(["en-US"])); return Promise.resolve(["en-US"]); },
    },
    offscreen: {
      // MV3 service worker uses this to get a DOM context. Our bg lives
      // in the page realm so it already has DOM access; the document
      // already exists, but we still accept the call to avoid throwing.
      createDocument(_opts, cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
      closeDocument(cb) { if (typeof cb === "function") Promise.resolve().then(cb); return Promise.resolve(); },
      hasDocument(cb) { if (typeof cb === "function") Promise.resolve().then(() => cb(true)); return Promise.resolve(true); },
      Reason: { DOM_SCRAPING: "DOM_SCRAPING", DOM_PARSER: "DOM_PARSER", BLOBS: "BLOBS" },
    },
  };

  // browserAction is the MV2 alias for action. A common compatibility
  // dance in extensions that ship both MV2 and MV3 looks like:
  //   if (typeof ext_api.action !== 'object') ext_api.action = ext_api.browserAction;
  // We expose both so the alias check passes either way.
  chrome.browserAction = chrome.action;

  // Some extensions probe contextMenus as a feature flag; cheap stub.
  chrome.contextMenus = {
    create() { return 0; }, remove() {}, removeAll() {}, update() {},
    onClicked: { addListener() {}, removeListener() {} },
  };

  // Wire both names. Extensions commonly do
  // `const ext_api = chrome || browser;` with a follow-up
  // `if (!ext_chromium) ext_api = browser;`. Exposing both globals
  // means either branch lands on the same object.
  globalThis.chrome = chrome;
  globalThis.browser = chrome;

  // ----- synchronous-drainable setTimeout/setInterval ------------------
  // Extensions commonly fan out per-tab work across N setTimeouts
  // (e.g. 5 retries spaced 200ms apart, plus setTimeout(fn,0) for
  // various deferred bookkeeping). Obscura's bootstrap.js implements
  // setTimeout via Promise.resolve().then(), which means timers fire
  // whenever V8 drains microtasks — which happens BETWEEN script
  // evaluations, not during one. Our preload injects everything as a
  // single execute_script, so without a synchronous drain the timers
  // never get a chance to run before Rust returns to dump_text.
  //
  // We override setTimeout/setInterval (only for the duration of this
  // realm) with a queue, then expose `__obscura_ext_drain_timers(maxIters)`
  // which the preload IIFE calls right after firing the onUpdated event.
  // The drain loop pops timers in FIFO order, lets them enqueue more,
  // and stops when the queue is empty or we hit `maxIters` (sanity cap).
  const _orig_setTimeout = globalThis.setTimeout;
  const _orig_setInterval = globalThis.setInterval;
  const _orig_clearTimeout = globalThis.clearTimeout;
  let _timer_id = 1_000_000;
  const _timer_queue = []; // [{id, fn, args}]
  const _timer_cancelled = new Set();
  globalThis.setTimeout = function (fn, _delay, ...args) {
    if (typeof fn !== "function") return ++_timer_id;
    const id = ++_timer_id;
    _timer_queue.push({ id, fn, args });
    if (globalThis.__obscura_ext_trace_timers) {
      // Capture a short stack snippet so we know WHICH bg.js path queued.
      const stack = (new Error()).stack || "";
      const snippet = stack.split("\n").slice(2, 4).join(" | ").slice(0, 200);
      console.log("obscura-ext: setTimeout queued #" + id + " delay=" + _delay + " from: " + snippet);
    }
    return id;
  };
  globalThis.setInterval = function (fn, delay, ...args) {
    // Real-world MV3 extensions tend to use setInterval only inside
    // service-worker keepalive loops, which we don't need (we run
    // bg as a persistent script). Translate to a single-shot to match
    // the runtime expectations: the keepalive's next tick is meaningless
    // here, but the side-effect inside the callback still runs once.
    return globalThis.setTimeout(fn, delay, ...args);
  };
  globalThis.clearTimeout = function (id) {
    _timer_cancelled.add(id);
    if (typeof _orig_clearTimeout === "function") _orig_clearTimeout(id);
  };
  globalThis.clearInterval = globalThis.clearTimeout;
  globalThis.__obscura_ext_pending_timer_count = function () { return _timer_queue.length; };
  globalThis.__obscura_ext_drain_timers = function (maxIters) {
    maxIters = maxIters || 50;
    let drained = 0;
    let iters = 0;
    while (_timer_queue.length && iters < maxIters) {
      const t = _timer_queue.shift();
      if (_timer_cancelled.has(t.id)) { continue; }
      if (globalThis.__obscura_ext_trace_timers) {
        const body = String(t.fn).slice(0, 200).replace(/\s+/g, " ");
        console.log("obscura-ext: drain #" + t.id + " body=" + body);
      }
      try {
        t.fn.apply(null, t.args || []);
      } catch (e) {
        console.error("obscura-ext: timer threw:", e && e.stack || e);
      }
      drained++;
      iters++;
    }
    if (_timer_queue.length) {
      console.warn("obscura-ext: drain cap hit, " + _timer_queue.length + " timers still pending");
    }
    return drained;
  };

  // Hook the host calls.
  globalThis.__obscura_ext_onupdated_count = function () { return _onUpdatedListeners.length; };
  globalThis.__obscura_ext_fire_loaded = function (url) {
    const tab = _currentTab();
    tab.url = url;
    // changeInfo intentionally does NOT include `url`. Real-world
    // listeners commonly gate "is this a new URL?" off changeInfo.url
    // and use a +500ms delay when set, which is meaningless in our
    // synchronous drain but burns a slot in the timer queue. Setting
    // status:"complete" is the signal that triggers the listener's
    // per-page work.
    const changeInfo = { status: "complete" };
    for (const fn of _onUpdatedListeners.slice()) {
      try {
        fn(tab.id, changeInfo, tab);
      } catch (e) {
        console.error("obscura-ext: onUpdated listener threw:", e && e.stack || e);
      }
    }
  };

  globalThis.__obscura_ext_dispatch_message = _dispatchMessage;
})();
