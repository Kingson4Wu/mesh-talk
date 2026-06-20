import { describe, it, expect, vi, beforeEach } from "vitest";

const { invoke } = vi.hoisted(() => ({ invoke: vi.fn() }));
vi.mock("@tauri-apps/api/core", () => ({ invoke }));

// Capture the event handlers start() registers, so we can drive inbound events.
const { captured } = vi.hoisted(() => ({
  captured: { current: null as null | Record<string, (e: unknown) => void> },
}));
vi.mock("@/lib/events", () => ({
  subscribeNodeEvents: (h: Record<string, (e: unknown) => void>) => {
    captured.current = h;
    return () => {};
  },
}));

import { useChat, convKey } from "./chat";

const reset = () =>
  useChat.setState({
    ready: false,
    myId: "me",
    myAccountId: "myacct",
    peers: [],
    accounts: [],
    channels: [],
    active: null,
    messages: {},
    reactions: {},
    unread: {},
    members: [],
    incomingFiles: [],
  });

beforeEach(() => {
  invoke.mockReset();
  reset();
});

describe("convKey", () => {
  it("namespaces by kind + id", () => {
    expect(convKey({ kind: "account", id: "a1", name: "x" })).toBe("account:a1");
    expect(convKey({ kind: "channel", id: "c1", name: "y" })).toBe("channel:c1");
  });
});

describe("open", () => {
  it("loads history + reactions, clears unread, sets active", async () => {
    invoke.mockImplementation((cmd: string) => {
      if (cmd === "account_history")
        return Promise.resolve([
          { id: "e1", from_me: false, who: "alice", text: "hello", wall_clock: 1000, reply_to: null },
        ]);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    useChat.setState({ unread: { "account:a1": 3 } });
    await useChat.getState().open({ kind: "account", id: "a1", name: "A" });

    const s = useChat.getState();
    expect(s.active).toEqual({ kind: "account", id: "a1", name: "A" });
    expect(s.messages["account:a1"]).toHaveLength(1);
    expect(s.messages["account:a1"][0].text).toBe("hello");
    expect(s.unread["account:a1"]).toBe(0);
  });
});

describe("send", () => {
  it("sends then reloads so the message gets its real id", async () => {
    let history: unknown[] = [];
    invoke.mockImplementation((cmd: string, args?: unknown) => {
      if (cmd === "send_to_account") {
        const a = args as { text: string };
        history = [{ id: "e1", from_me: true, who: "me", text: a.text, wall_clock: 1, reply_to: null }];
        return Promise.resolve();
      }
      if (cmd === "account_history") return Promise.resolve(history);
      if (cmd === "account_reactions") return Promise.resolve([]);
      return Promise.resolve(undefined);
    });
    useChat.setState({ active: { kind: "account", id: "a1", name: "A" } });
    await useChat.getState().send("hi", null);

    expect(invoke).toHaveBeenCalledWith("send_to_account", {
      account: "a1",
      text: "hi",
      replyTo: null,
    });
    const msgs = useChat.getState().messages["account:a1"];
    expect(msgs).toHaveLength(1);
    expect(msgs[0]).toMatchObject({ id: "e1", text: "hi", fromMe: true });
    expect(msgs[0].pending).toBeFalsy();
  });

  it("ignores an empty message", async () => {
    useChat.setState({ active: { kind: "account", id: "a1", name: "A" } });
    await useChat.getState().send("   ", null);
    expect(invoke).not.toHaveBeenCalled();
  });
});

describe("toggleReaction", () => {
  beforeEach(() => {
    invoke.mockResolvedValue([]);
    useChat.setState({
      active: { kind: "account", id: "a1", name: "A" },
      reactions: { "account:a1": [{ target: "t1", emoji: "👍", who: ["me"] }] },
    });
  });
  it("removes a reaction I already made", async () => {
    await useChat.getState().toggleReaction("t1", "👍");
    expect(invoke).toHaveBeenCalledWith("react_account", {
      account: "a1",
      target: "t1",
      emoji: "👍",
      remove: true,
    });
  });
  it("adds a reaction I have not made", async () => {
    await useChat.getState().toggleReaction("t1", "🎉");
    expect(invoke).toHaveBeenCalledWith("react_account", {
      account: "a1",
      target: "t1",
      emoji: "🎉",
      remove: false,
    });
  });
});

describe("incoming events", () => {
  const peer = {
    user_id: "dev1",
    account_id: "acctA",
    name: "Alice",
    addr: "1.2.3.4:7000",
    post_office: false,
  };

  async function boot() {
    invoke.mockImplementation((cmd: string) => {
      switch (cmd) {
        case "my_id":
          return Promise.resolve("me");
        case "account_id":
          return Promise.resolve("myacct");
        case "list_peers":
          return Promise.resolve([peer]);
        default:
          return Promise.resolve([]);
      }
    });
    const stop = useChat.getState().start();
    await vi.waitFor(() => expect(useChat.getState().ready).toBe(true));
    return stop;
  }

  it("routes a DM to the sender's ACCOUNT conversation and bumps unread", async () => {
    const stop = await boot();
    captured.current!.onDm({
      from: "dev1",
      from_name: "Alice",
      text: "hi",
      reply_to: null,
    });
    expect(useChat.getState().unread["account:acctA"]).toBe(1);
    stop();
  });

  it("does not bump unread for a DM from an undiscovered peer", async () => {
    const stop = await boot();
    captured.current!.onDm({ from: "ghost", from_name: "?", text: "hi", reply_to: null });
    expect(useChat.getState().unread["account:ghost"]).toBeUndefined();
    stop();
  });

  it("routes a channel message to its channel conversation", async () => {
    const stop = await boot();
    captured.current!.onChannelMessage({
      channel_id: "c1",
      channel_name: "general",
      from: "dev1",
      text: "yo",
      reply_to: null,
    });
    expect(useChat.getState().unread["channel:c1"]).toBe(1);
    stop();
  });

  it("de-dupes received files by file_conv", async () => {
    const stop = await boot();
    const f = { conv: "x", from: "dev1", name: "a.pdf", size: 10, file_conv: "fc1" };
    captured.current!.onFile(f);
    captured.current!.onFile(f);
    expect(useChat.getState().incomingFiles).toHaveLength(1);
    expect(useChat.getState().incomingFiles[0].fromName).toBe("Alice");
    stop();
  });
});
