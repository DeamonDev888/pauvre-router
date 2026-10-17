// pauvre-router — plugin desktop Hermes : monitoring du router des pauvres.
// Pane live : santé des modèles, cooldowns, stats d'usage, spam récent.
import {
  host,
  jsx,
  useValue,
  useQuery,
  Badge,
  StatusDot,
  EmptyState,
} from "@hermes/plugin-sdk";

const BASE = "http://127.0.0.1:4445";

async function fetchStats() {
  const r = await fetch(BASE + "/admin/stats");
  if (!r.ok) throw new Error("router injoignable (" + r.status + ")");
  return r.json();
}

function secs(v) {
  if (!v) return null;
  if (v >= 3600) return Math.floor(v / 3600) + "h" + Math.floor((v % 3600) / 60) + "m";
  if (v >= 60) return Math.floor(v / 60) + "m" + (v % 60) + "s";
  return v + "s";
}

function ModelRow(props) {
  const h = props.health;
  const cooling = h && h.cooldown_secs > 0;
  return jsx("div", {
    style: {
      display: "flex",
      alignItems: "center",
      gap: "8px",
      padding: "6px 0",
      borderBottom: "1px solid var(--ui-stroke-secondary)",
      fontSize: "12px",
    },
    children: [
      jsx(StatusDot, { kind: cooling ? "warn" : "ok" }),
      jsx("span", {
        style: { flex: "1", fontFamily: "var(--ui-font-mono, monospace)" },
        children: props.model,
      }),
      h && h.last_latency_ms
        ? jsx("span", { style: { color: "var(--ui-text-tertiary)" }, children: h.last_latency_ms + "ms" })
        : null,
      h && h.ok ? jsx(Badge, { children: h.ok + " ok" }) : null,
      cooling
        ? jsx(Badge, { kind: "warn", children: "cooldown " + secs(h.cooldown_secs) })
        : null,
    ],
  });
}

function StatsGrid(props) {
  const s = props.stats || {};
  const cells = [
    ["requêtes", s.requests_total],
    ["ok", s.ok_total],
    ["fallback", s.fallback_total],
    ["cache hits", s.cache_hits],
    ["spam bloqué", s.spam_blocked],
    ["erreurs", s.errors_total],
  ];
  return jsx("div", {
    style: {
      display: "grid",
      gridTemplateColumns: "1fr 1fr 1fr",
      gap: "8px",
      padding: "8px 0",
    },
    children: cells.map(([label, val]) =>
      jsx("div", {
        style: {
          border: "1px solid var(--ui-stroke-secondary)",
          borderRadius: "6px",
          padding: "6px 8px",
        },
        children: [
          jsx("div", {
            style: { fontSize: "10px", color: "var(--ui-text-tertiary)", textTransform: "uppercase" },
            children: label,
          }),
          jsx("div", { style: { fontSize: "16px", fontWeight: "600" }, children: String(val ?? 0) }),
        ],
      })
    ),
  });
}

export function register(ctx) {
  ctx.register({
    id: "pauvre-pane",
    area: "panes",
    title: "Router des Pauvres",
    data: { placement: "right" },
    render() {
      const q = useQuery({
        queryKey: ["pauvre-stats"],
        queryFn: fetchStats,
        refetchInterval: 5000,
        retry: 1,
      });
      const data = q.data;
      const models = data && data.health ? Object.keys(data.health) : [];
      if (q.isError) {
        return jsx(EmptyState, {
          icon: "plug-disconnected",
          label: "Router des Pauvres hors ligne (127.0.0.1:4445)",
        });
      }
      return jsx("div", {
        style: { padding: "10px", overflow: "auto", height: "100%" },
        children: [
          jsx("div", {
            style: { display: "flex", alignItems: "center", gap: "6px", marginBottom: "4px" },
            children: [
              jsx("span", { style: { fontWeight: "600" }, children: "🗑️ Router des Pauvres" }),
              jsx(Badge, { children: "v" + (data ? data.version : "?") }),
              data && data.mode ? jsx(Badge, { kind: data.mode === "roundrobin" ? "info" : "default", children: data.mode }) : null,
              data ? jsx(Badge, { children: "in-flight " + data.in_flight }) : null,
            ],
          }),
          jsx(StatsGrid, { stats: data && data.stats }),
          jsx("div", {
            style: { fontSize: "11px", color: "var(--ui-text-tertiary)", margin: "6px 0 2px" },
            children: "SANTÉ DES MODÈLES",
          }),
          jsx("div", {
            children: models.map((m) =>
              jsx(ModelRow, { key: m, model: m, health: data.health[m] })
            ),
          }),
          data && data.recent_spam && data.recent_spam.length
            ? jsx("div", {
                children: [
                  jsx("div", {
                    style: { fontSize: "11px", color: "var(--ui-text-tertiary)", margin: "8px 0 2px" },
                    children: "SPAM RÉCENT",
                  }),
                  jsx("div", {
                    style: { fontSize: "11px", fontFamily: "var(--ui-font-mono, monospace)" },
                    children: data.recent_spam
                      .map((e) => e.ip + " @ " + new Date(e.at_epoch * 1000).toLocaleTimeString())
                      .join("\n"),
                  }),
                ],
              })
            : null,
        ],
      });
    },
  });

  ctx.register({
    id: "pauvre-open",
    area: "palette",
    title: "Router des Pauvres : montrer le pane de monitoring",
    run() {
      host.navigate("/"); // le pane est déjà actif ; commande placeholder d'accès rapide
    },
  });
}

export default { id: "pauvre-router", name: "Router des Pauvres", register };
