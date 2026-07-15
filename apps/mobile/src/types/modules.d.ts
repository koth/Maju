// Ambient module shims for transitive deps that ship no TypeScript types.
// `react-native-markdown-display`'s bundled `.d.ts` imports `markdown-it`,
// which has no types in this tree. `skipLibCheck` already suppresses errors
// inside the library's own declaration file; these shims make the types
// resolve explicitly so the app code that imports the markdown component
// type-checks cleanly even if skipLibCheck is later disabled.

declare module "markdown-it" {
  const MarkdownIt: {
    new (opts?: Record<string, unknown>): MarkdownIt;
    (opts?: Record<string, unknown>): MarkdownIt;
  };
  interface MarkdownIt {
    parse(src: string, env?: unknown): unknown[];
    render(src: string): string;
    use(plugin: unknown, ...args: unknown[]): MarkdownIt;
  }
  export default MarkdownIt;
}

declare module "markdown-it/lib/token" {
  export default class Token {
    type: string;
    tag: string;
    attrs: Array<[string, string]> | null;
    map: Array<number> | null;
    nesting: number;
    level: number;
    children: Token[] | null;
    content: string;
    markup: string;
    info: string;
    meta: unknown | null;
    block: boolean;
    hidden: boolean;
  }
}
// end of file
