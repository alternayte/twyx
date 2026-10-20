// twyx integration {version}. `twyx integration remove {agent}` deletes this file.
// It reports OpenCode's state and session to the twyx pane OpenCode runs in,
// through `twyx notify`. Outside a twyx pane it does nothing.
import { execFile } from "node:child_process";

export const Twyx = async () => {
  if (!process.env.TWYX_PANE_ID) return {};
  let queue: Promise<unknown> = Promise.resolve();
  const notify = (event: string, session?: string) => {
    const args = ["notify", event, "--agent", "{agent}"];
    if (session) args.push("--session", session);
    // One at a time, so twyx sees the events in order.
    queue = queue.then(() => new Promise((done) => execFile("twyx", args, () => done(null))));
  };
  // Subagents run in child sessions; their turns are not the pane's.
  const children = new Set<string>();
  const own = (session?: string) => !session || !children.has(session);
  return {
    event: async ({ event }: any) => {
      const p = event.properties ?? {};
      switch (event.type) {
        case "session.created":
        case "session.updated":
          if (p.info?.parentID) children.add(p.info.id);
          break;
        case "session.status":
          if (p.status?.type === "busy" && own(p.sessionID)) notify("working", p.sessionID);
          break;
        case "session.idle":
          if (own(p.sessionID)) notify("turn-finished", p.sessionID);
          break;
        case "session.error":
          if (own(p.sessionID)) notify("error", p.sessionID);
          break;
        case "permission.asked":
        case "permission.updated":
        case "question.asked":
          notify("awaiting-approval");
          break;
        case "permission.replied":
        case "question.replied":
        case "question.rejected":
          notify("working");
          break;
      }
    },
  };
};
