// Generic Boa bridge. API methods and their types come from the Rust schema.
(() => {
  const fail = __slerm_error;
  let plugin;
  const client = call => new Proxy(Object.create(null), {
    get: (_, method) => method === "then" ? undefined : async (params = {}) => {
      const reply = JSON.parse(call(JSON.stringify({ ...params, method })));
      if (reply.error !== undefined) throw new Error(reply.error);
      return reply.result;
    },
  });
  const api = client(__slerm_call);
  const fs = client(__slerm_fs);
  const invoke = (callback, ...args) => {
    if (callback) Promise.resolve().then(() => callback(...args)).catch(error => fail(String(error)));
  };
  Object.defineProperty(globalThis, "__slerm", { value: Object.freeze({
    fs,
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
})();
