import * as React from "react";
import { useEffect, useRef } from "react";
import {
  Editor,
  defaultValueCtx,
  editorViewCtx,
  editorViewOptionsCtx,
  rootCtx,
} from "@milkdown/core";
import { listener, listenerCtx } from "@milkdown/plugin-listener";
import { commonmark } from "@milkdown/preset-commonmark";
import { Milkdown, MilkdownProvider, useEditor } from "@milkdown/react";

import { cn } from "@/lib/utils";

export interface MarkdownEditorProps {
  value: string;
  onChange: (md: string) => void;
  readOnly?: boolean;
  className?: string;
  placeholder?: string;
}

function MarkdownEditorInner({
  value,
  onChange,
  readOnly = false,
  className,
  placeholder,
}: MarkdownEditorProps) {
  // Read the latest `onChange` through a ref so the listener stays current
  // without recreating the editor (which would drop the ProseMirror buffer).
  const onChangeRef = useRef(onChange);
  useEffect(() => {
    onChangeRef.current = onChange;
  }, [onChange]);

  // `editable` is a predicate ProseMirror re-evaluates on each state update;
  // reading `readOnly` from a ref keeps it live across prop changes without a
  // teardown/recreate.
  const readOnlyRef = useRef(readOnly);
  useEffect(() => {
    readOnlyRef.current = readOnly;
  }, [readOnly]);

  // `value` seeds the document once, on mount. The editor is uncontrolled
  // thereafter (Milkdown/ProseMirror own the buffer) — to load a different
  // document, remount the component via a React `key`.
  const { get } = useEditor((root) =>
    Editor.make()
      .config((ctx) => {
        ctx.set(rootCtx, root);
        ctx.set(defaultValueCtx, value);
        ctx.update(editorViewOptionsCtx, (prev) => ({
          ...prev,
          editable: () => !readOnlyRef.current,
        }));
        ctx.get(listenerCtx).markdownUpdated((_, markdown, prevMarkdown) => {
          if (markdown !== prevMarkdown) onChangeRef.current(markdown);
        });
      })
      .use(commonmark)
      .use(listener),
  );

  // Force ProseMirror to re-read the `editable` predicate when `readOnly` flips.
  useEffect(() => {
    get()?.action((ctx) => {
      const view = ctx.get(editorViewCtx);
      view.updateState(view.state);
    });
  }, [readOnly, get]);

  // Placeholder is rendered via CSS (see `.ff-md-editor` in index.css); the text
  // is passed as a custom property so no extra plugin/dependency is needed.
  const style = placeholder
    ? ({
        "--ff-md-placeholder": JSON.stringify(placeholder),
      } as React.CSSProperties)
    : undefined;

  return (
    <div
      data-slot="markdown-editor"
      data-readonly={readOnly || undefined}
      className={cn("ff-md-editor", className)}
      style={style}
    >
      <Milkdown />
    </div>
  );
}

// Milkdown's React bindings require the surrounding <MilkdownProvider>; wrapping
// it here keeps this primitive self-contained for callers.
export function MarkdownEditor(props: MarkdownEditorProps) {
  return (
    <MilkdownProvider>
      <MarkdownEditorInner {...props} />
    </MilkdownProvider>
  );
}
