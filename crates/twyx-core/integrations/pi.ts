// twyx integration {version}. `twyx integration remove {agent}` deletes this file.
// It reports Pi's state and session to the twyx pane Pi runs in, through
// `twyx notify`. Outside a twyx pane it does nothing.
import { execFile } from "node:child_process";

export default function (pi: any) {
  if (!process.env.TWYX_PANE_ID) return;
  let queue: Promise<unknown> = Promise.resolve();
  const notify = (event: string, session?: string) => {
    const args = ["notify", event, "--agent", "{agent}"];
    if (session) args.push("--session", session);
    // One at a time, so twyx sees the events in order.
    queue = queue.then(() => new Promise((done) => execFile("twyx", args, () => done(null))));
  };
  let failed = false;
  pi.on("session_start", (_event: any, ctx: any) => {
    notify("session", ctx.sessionManager.getSessionId());
  });
  pi.on("agent_start", () => {
    failed = false;
    notify("working");
  });
  pi.on("agent_end", (event: any) => {
    failed = event.messages?.at(-1)?.stopReason === "error";
  });
  pi.on("ui_prompt_start", () => notify("awaiting-approval"));
  pi.on("ui_prompt_end", () => notify("working"));
  pi.on("agent_settled", () => notify(failed ? "error" : "turn-finished"));
}
