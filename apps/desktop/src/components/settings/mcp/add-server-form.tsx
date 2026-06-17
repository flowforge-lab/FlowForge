import { useState } from "react";
import { Plus, X } from "lucide-react";
import { Button } from "@/components/ui/button";
import { Input } from "@/components/ui/input";
import { useMcpStore } from "@/store/mcp";

/** Collapsible form to add an MCP server definition (id + command + args). */
export function AddServerForm() {
  const add = useMcpStore((s) => s.add);
  const [open, setOpen] = useState(false);
  const [id, setId] = useState("");
  const [command, setCommand] = useState("");
  const [args, setArgs] = useState("");

  const trimmedId = id.trim();
  const trimmedCommand = command.trim();
  const valid = trimmedId !== "" && trimmedCommand !== "";

  function reset() {
    setId("");
    setCommand("");
    setArgs("");
    setOpen(false);
  }

  function submit() {
    if (!valid) return;
    void add({
      id: trimmedId,
      command: trimmedCommand,
      // Whitespace-separated tokens; empty → no args.
      args: args.trim() === "" ? [] : args.trim().split(/\s+/),
      env: {},
      disabled: false,
    });
    reset();
  }

  if (!open) {
    return (
      <Button variant="outline" size="sm" onClick={() => setOpen(true)}>
        <Plus />
        Add server
      </Button>
    );
  }

  return (
    <form
      className="space-y-3 rounded-lg border border-border bg-muted/30 p-3"
      onSubmit={(e) => {
        e.preventDefault();
        submit();
      }}
    >
      <div className="flex items-center justify-between">
        <h4 className="text-[12px] font-medium text-foreground">Add server</h4>
        <Button
          type="button"
          variant="ghost"
          size="icon-xs"
          onClick={reset}
          title="Cancel"
        >
          <X />
        </Button>
      </div>

      <div className="space-y-1.5">
        <label
          htmlFor="mcp-add-id"
          className="text-[11px] text-muted-foreground"
        >
          Server id
        </label>
        <Input
          id="mcp-add-id"
          value={id}
          placeholder="github"
          autoComplete="off"
          onChange={(e) => setId(e.target.value)}
        />
      </div>

      <div className="space-y-1.5">
        <label
          htmlFor="mcp-add-command"
          className="text-[11px] text-muted-foreground"
        >
          Command
        </label>
        <Input
          id="mcp-add-command"
          value={command}
          placeholder="npx"
          autoComplete="off"
          onChange={(e) => setCommand(e.target.value)}
        />
      </div>

      <div className="space-y-1.5">
        <label
          htmlFor="mcp-add-args"
          className="text-[11px] text-muted-foreground"
        >
          Arguments
        </label>
        <Input
          id="mcp-add-args"
          value={args}
          placeholder="-y @modelcontextprotocol/server-github"
          autoComplete="off"
          onChange={(e) => setArgs(e.target.value)}
        />
        <p className="text-[11px] leading-relaxed text-muted-foreground">
          Space-separated. Environment variables can be edited in{" "}
          <code className="text-[10px]">mcp.json</code>.
        </p>
      </div>

      <div className="flex justify-end gap-1.5">
        <Button type="button" variant="ghost" size="sm" onClick={reset}>
          Cancel
        </Button>
        <Button type="submit" size="sm" disabled={!valid}>
          Add
        </Button>
      </div>
    </form>
  );
}
