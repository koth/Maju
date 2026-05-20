declare module "ghostty-web" {
  export interface Disposable {
    dispose(): void;
  }

  export interface TerminalTheme {
    background?: string;
    foreground?: string;
    cursor?: string;
    selection?: string;
    black?: string;
    red?: string;
    green?: string;
    yellow?: string;
    blue?: string;
    magenta?: string;
    cyan?: string;
    white?: string;
    brightBlack?: string;
    brightRed?: string;
    brightGreen?: string;
    brightYellow?: string;
    brightBlue?: string;
    brightMagenta?: string;
    brightCyan?: string;
    brightWhite?: string;
  }

  export interface TerminalOptions {
    cursorBlink?: boolean;
    fontFamily?: string;
    fontSize?: number;
    scrollback?: number;
    theme?: TerminalTheme;
  }

  export interface TerminalDimensions {
    cols: number;
    rows: number;
  }

  export interface TerminalAddon {
    activate?(terminal: Terminal): void;
    dispose?(): void;
  }

  export class Terminal {
    cols: number;
    rows: number;
    textarea?: HTMLTextAreaElement | null;

    constructor(options?: TerminalOptions);
    dispose(): void;
    focus(): void;
    loadAddon(addon: TerminalAddon): void;
    onData(callback: (data: string) => void): Disposable;
    onResize(callback: (dimensions: TerminalDimensions) => void): Disposable;
    open(element: HTMLElement): void;
    paste(data: string): void;
    write(data: string): void;
  }

  export class FitAddon implements TerminalAddon {
    activate(terminal: Terminal): void;
    dispose(): void;
    fit(): void;
    observeResize?(): void;
    proposeDimensions?(): TerminalDimensions | undefined;
  }

  export function init(): Promise<void>;
}
