window.__ModuleLoader__.load({
  id: '@local/susi-cockpit',
  factory(require) {
    const React = require('react');
    const h = React.createElement;

    // Two cockpits, two sidebar entries, two panels. They are different
    // surfaces on purpose:
    //
    // - external: the standalone app that drives the external orchestration
    //   harness (the former litellm/openclaw cockpit, split into its own
    //   application).
    // - internal: susi's own CEO cockpit, built entirely in-house in Rust and
    //   served by `susi harness cockpit`.
    //
    // Each keeps its own URL in the browser, so repointing one never moves the
    // other. The internal one also adopts the legacy single-cockpit key: before
    // there were two, `susi.cockpit.url` held that panel's URL (with
    // `http://127.0.0.1:9181/?embed=1` as the old default), and a URL an
    // operator saved there must not be lost.
    const COCKPITS = [
      {
        id: 'susi-cockpit-external',
        kind: 'external',
        label: () => 'CEO cockpit (external)',
        title: 'CEO cockpit · external orchestration',
        defaultUrl: 'http://localhost:8080/',
        storageKey: 'susi.cockpit.external.url',
        legacyKey: null,
        hint: 'Start the external orchestration cockpit app (default http://localhost:8080/), then reload.',
      },
      {
        id: 'susi-cockpit-internal',
        kind: 'internal',
        label: () => 'CEO cockpit (susi native)',
        title: 'CEO cockpit · susi native',
        defaultUrl: 'http://127.0.0.1:9181/?embed=1',
        storageKey: 'susi.cockpit.internal.url',
        legacyKey: 'susi.cockpit.url',
        hint: 'Start susi\u2019s own panel: susi harness cockpit --ledger out/ledger.jsonl --plan plan.json --controls out/control.jsonl --port 9181',
      },
    ];

    // The pre-two-cockpit default; it is also the internal cockpit's default, so
    // a browser that saved it simply agrees with the default.
    const LEGACY_DEFAULT = 'http://127.0.0.1:9181/?embed=1';

    function readUrl(cockpit) {
      try {
        const stored = window.localStorage.getItem(cockpit.storageKey);
        if (stored) return stored;
        if (cockpit.legacyKey) {
          const legacy = window.localStorage.getItem(cockpit.legacyKey);
          if (legacy && legacy !== LEGACY_DEFAULT) return legacy;
        }
        return cockpit.defaultUrl;
      } catch (error) {
        return cockpit.defaultUrl;
      }
    }

    function storeUrl(cockpit, url) {
      try {
        window.localStorage.setItem(cockpit.storageKey, url);
      } catch (error) {
        // A blocked localStorage only loses the preference, not the panel.
      }
    }

    function Icon({ size = 18, active = false, kind = 'external' }) {
      const stroke = active ? 'var(--dsw-alias-brand-primary)' : 'currentColor';
      const shared = {
        viewBox: '0 0 24 24',
        width: size,
        height: size,
        fill: 'none',
        stroke,
        strokeWidth: 1.7,
        strokeLinecap: 'round',
        strokeLinejoin: 'round',
        'aria-hidden': true,
        style: { display: 'block' },
      };
      // The gauge for the external harness; the same gauge enclosed in a ring
      // for susi's own cockpit, so the two read as related but distinct.
      const dial = [
        h('path', { key: 'arc', d: 'M3.5 17.5a8.5 8.5 0 1 1 17 0' }),
        h('path', { key: 'needle', d: 'M12 17.5l4.2-5.4' }),
        h('circle', {
          key: 'hub',
          cx: 12,
          cy: 17.5,
          r: 1.5,
          fill: stroke,
          stroke: 'none',
        }),
      ];
      if (kind === 'internal') {
        return h('svg', shared, [
          h('rect', {
            key: 'ring',
            x: 1.6,
            y: 1.6,
            width: 20.8,
            height: 20.8,
            rx: 6.4,
            opacity: 0.55,
          }),
          ...dial,
        ]);
      }
      return h('svg', shared, dial);
    }

    const buttonStyle = {
      font: 'inherit',
      fontSize: 12,
      color: 'var(--dsw-alias-label-primary)',
      background: 'var(--dsw-alias-bg-layer-2)',
      border: '1px solid var(--dsw-alias-border-l2)',
      borderRadius: 6,
      padding: '5px 10px',
      cursor: 'pointer',
    };

    function makeCockpitPanel(cockpit) {
      return function CockpitPanel() {
        const [url, setUrl] = React.useState(() => readUrl(cockpit));
        const [draft, setDraft] = React.useState(() => readUrl(cockpit));
        const [ready, setReady] = React.useState(false);
        const [generation, setGeneration] = React.useState(0);

        const reload = () => {
          setReady(false);
          setGeneration((value) => value + 1);
        };
        const save = () => {
          const next = draft.trim() || cockpit.defaultUrl;
          storeUrl(cockpit, next);
          setUrl(next);
          setDraft(next);
          reload();
        };

        return h(
          'div',
          {
            style: {
              display: 'flex',
              flexDirection: 'column',
              height: '100%',
              minHeight: 0,
              background: 'var(--dsw-alias-bg-base)',
              color: 'var(--dsw-alias-label-primary)',
            },
          },
          h(
            'div',
            {
              style: {
                display: 'flex',
                alignItems: 'center',
                gap: 8,
                padding: '10px 12px',
                borderBottom: '1px solid var(--dsw-alias-border-l1)',
                flex: '0 0 auto',
              },
            },
            h(
              'strong',
              {
                style: {
                  fontSize: 12,
                  letterSpacing: '.08em',
                  textTransform: 'uppercase',
                  whiteSpace: 'nowrap',
                },
              },
              cockpit.title,
            ),
            h('input', {
              value: draft,
              onChange: (event) => setDraft(event.target.value),
              onKeyDown: (event) => {
                if (event.key === 'Enter') save();
              },
              'aria-label': `Cockpit URL (${cockpit.kind})`,
              spellCheck: false,
              style: {
                font: 'inherit',
                fontSize: 12,
                color: 'var(--dsw-alias-label-primary)',
                background: 'var(--dsw-alias-bg-layer-2)',
                border: '1px solid var(--dsw-alias-border-l1)',
                borderRadius: 6,
                padding: '5px 8px',
                width: 'min(420px, 45vw)',
              },
            }),
            h('button', { type: 'button', onClick: save, style: buttonStyle }, 'save'),
            h('button', { type: 'button', onClick: reload, style: buttonStyle }, 'reload'),
            h(
              'a',
              {
                href: url,
                target: '_blank',
                rel: 'noreferrer',
                style: { ...buttonStyle, textDecoration: 'none' },
              },
              'open',
            ),
            h(
              'span',
              {
                style: {
                  marginLeft: 'auto',
                  fontSize: 11,
                  color: ready
                    ? 'var(--dsw-alias-state-success-primary)'
                    : 'var(--dsw-alias-label-secondary)',
                },
              },
              ready ? 'connected' : 'waiting for the cockpit',
            ),
          ),
          h(
            'div',
            { style: { position: 'relative', flex: '1 1 auto', minHeight: 0 } },
            h('iframe', {
              key: generation,
              src: url,
              title: `SUSI ${cockpit.kind} cockpit`,
              onLoad: () => setReady(true),
              style: { width: '100%', height: '100%', border: 0, display: 'block' },
            }),
            !ready &&
              h(
                'div',
                {
                  style: {
                    position: 'absolute',
                    inset: 0,
                    display: 'flex',
                    alignItems: 'center',
                    justifyContent: 'center',
                    padding: 24,
                    textAlign: 'center',
                    color: 'var(--dsw-alias-label-secondary)',
                    fontSize: 12,
                    lineHeight: 1.7,
                    pointerEvents: 'none',
                  },
                },
                cockpit.hint,
                h('br'),
                url,
              ),
          ),
        );
      };
    }

    return {
      inject: ['slots'],
      apply(ctx) {
        for (const [index, cockpit] of COCKPITS.entries()) {
          ctx.slots.inject('sidebar.panellist', () =>
            ctx.slots.register(
              {
                name: 'sidebar.panellist',
                id: cockpit.id,
                order: 20 + index,
                label: cockpit.label,
              },
              (props) => h(Icon, { ...props, kind: cockpit.kind }),
            ),
          );
          ctx.slots.inject('main', () =>
            ctx.slots.register(
              {
                name: 'main',
                key: cockpit.id,
              },
              makeCockpitPanel(cockpit),
            ),
          );
        }
      },
    };
  },
});
