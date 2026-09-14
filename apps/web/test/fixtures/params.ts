/**
 * Route `params` for dynamic App Router pages under test. The App Router
 * hands pages a promise; a settled thenable lets `use(params)` resolve
 * synchronously so the page renders on the first pass.
 */
export function routeParams<T extends object>(value: T): Promise<T> {
  const settled = Promise.resolve(value) as Promise<T> & {
    status?: "fulfilled";
    value?: T;
  };
  settled.status = "fulfilled";
  settled.value = value;
  return settled;
}
