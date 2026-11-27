/** True in `scripts/admin-build --dev`: the client talks to api/mock.ts, not the Worker. */
declare const MOCK: boolean;

/** Text fixtures (a post's Markdown) load as strings (esbuild --loader:.txt=text). */
declare module "*.txt" {
  const text: string;
  export default text;
}
