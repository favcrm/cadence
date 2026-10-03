// A tiny sandboxed chat screen (CAD-1111, Tier 2). It opts into the chat
// directive push, then shows the pushed text as text. It never fetches and
// has no links: the host gives it nothing else.
(function () {
  var boot = JSON.parse(document.getElementById("cadence-screen-boot").textContent);
  var root = document.getElementById("root");
  var channel = new MessageChannel();
  var port = channel.port1;
  function show(data) {
    root.textContent = "";
    var card = document.createElement("div");
    card.style.cssText = "font:14px system-ui,sans-serif;padding:12px;border:1px solid #888;border-radius:8px";
    var head = document.createElement("strong");
    head.textContent = "Post preview";
    var body = document.createElement("p");
    body.id = "preview-text";
    body.textContent = typeof data.text === "string" ? data.text : "";
    card.appendChild(head);
    card.appendChild(body);
    root.appendChild(card);
  }
  port.onmessage = function (event) {
    var m = event.data;
    if (m && m.v === 1 && m.op === "directive" && m.data) show(m.data);
  };
  parent.postMessage(
    { v: 1, op: "init", tag: boot.tag, bridge_nonce: boot.bridge_nonce, generation: boot.generation },
    boot.parent_origin,
    [channel.port2]
  );
  port.postMessage({ v: 1, op: "ready", accepts: ["chat-directive.v1"] });
})();
