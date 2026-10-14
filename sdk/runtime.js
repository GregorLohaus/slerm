// Generic Boa bridge. API methods and their types come from the Rust schema.
(() => {
  const fail = __slerm_error;
  let plugin;
  // Slow host calls answer { pending: id } and settle later via __slerm_complete.
  const pending = new Map();
  const settle = (resolve, reject, reply) => reply.error !== undefined ? reject(new Error(reply.error)) : resolve(reply.result);
  const client = call => new Proxy(Object.create(null), {
    get: (_, method) => method === "then" ? undefined : (params = {}) => new Promise((resolve, reject) => {
      const reply = JSON.parse(call(JSON.stringify({ method, params })));
      if (reply.pending !== undefined) pending.set(reply.pending, { resolve, reject });
      else settle(resolve, reject, reply);
    }),
  });
  const api = client(__slerm_call);
  const invoke = (callback, ...args) => {
    if (callback) Promise.resolve().then(() => callback(...args)).catch(error => fail(String(error)));
  };

  // Timers keep the handle across interval repeats; a cleared handle never fires.
  const timers = new Map();
  let nextTimer = 1;
  const schedule = (callback, ms, repeat, args) => {
    if (typeof callback !== "function") throw new TypeError("timer callback must be a function");
    const handle = nextTimer++;
    const delay = Math.max(repeat ? 10 : 0, Number(ms) || 0);
    timers.set(handle, { callback, delay, repeat, args });
    __slerm_timer(handle, delay);
    return handle;
  };
  const clear = handle => { timers.delete(handle); };
  for (const [name, value] of Object.entries({
    setTimeout: (callback, ms, ...args) => schedule(callback, ms, false, args),
    setInterval: (callback, ms, ...args) => schedule(callback, ms, true, args),
    clearTimeout: clear,
    clearInterval: clear,
  })) Object.defineProperty(globalThis, name, { value, writable: true, configurable: true });

  Object.defineProperty(globalThis, "__slerm", { value: Object.freeze({
    fs: client(__slerm_fs),
    system: client(__slerm_system),
    activate(definition, version) {
      if (version !== __slerm_version) throw new Error(`Unsupported Slerm API version ${version}`);
      if (plugin) throw new Error("A bundle must define exactly one plugin");
      plugin = definition;
      invoke(plugin.activate, api);
    },
  }) });
  Object.defineProperty(globalThis, "__slerm_dispatch", { value: event => {
    if (!plugin) throw new Error("Bundle did not call definePlugin");
    invoke(plugin.onEvent, event, api);
  } });
  Object.defineProperty(globalThis, "__slerm_complete", { value: (id, reply) => {
    const call = pending.get(id);
    pending.delete(id);
    if (call) settle(call.resolve, call.reject, reply);
  } });
  Object.defineProperty(globalThis, "__slerm_fire", { value: handle => {
    const timer = timers.get(handle);
    if (!timer) return;
    if (timer.repeat) __slerm_timer(handle, timer.delay);
    else timers.delete(handle);
    invoke(timer.callback, ...timer.args);
  } });
})();
