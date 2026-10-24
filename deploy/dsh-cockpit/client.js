window.__ModuleLoader__.load({
  id: '@local/susi-cockpit',
  factory(require) {
    const React = require('react');
    const h = React.createElement;
    // The CEO cockpit is its own standalone app now (the former
    // litellm/openclaw cockpit was split out); susi's own panel lives at the
    // harness cockpit's port and is a different surface. This panel embeds the
    // standalone one.
    const DEFAULT_URL = 'http://localhost:8080/';
    // The old harness-cockpit default, kept only so a browser that saved it
    // (before this change) starts following DEFAULT_URL instead of pinning the
    // stale port forever.
    const LEGACY_DEFAULT = 'http://127.0.0.1:9181/?embed=1';
    const STORAGE_KEY = 'susi.cockpit.url';

    function readUrl() {
      try {
        const stored = window.localStorage.getItem(STORAGE_KEY);
        if (stored && stored !== LEGACY_DEFAULT) return stored;
        return DEFAULT_URL;
      } catch (error) {
        return DEFAULT_URL;
      }
    }

    function storeUrl(url) {
      try {
        window.localStorage.setItem(STORAGE_KEY, url);
      } catch (error) {
        // A blocked localStorage only loses the preference, not the panel.
      }
    }

    function Icon({ size = 18, active = false }) {
      return h(
        'svg',
        {
          viewBox: '0 0 24 24',
          width: size,
          height: size,
          fill: 'none',
          stroke: active ? 'var(--dsw-alias-brand-primary)' : 'currentColor',
          strokeWidth: 1.7,
          strokeLinecap: 'round',
          strokeLinejoin: 'round',
          'aria-hidden': true,
          style: { display: 'block' },
        },
        h('path', { d: 'M3.5 17.5a8.5 8.5 0 1 1 17 0' }),
        h('path', { d: 'M12 17.5l4.2-5.4' }),
        h('circle', { cx: 12, cy: 17.5, r: 1.5, fill: 'currentColor', stroke: 'none' }),
      );
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

    function CockpitPanel() {
      const [url, setUrl] = React.useState(readUrl);
      const [draft, setDraft] = React.useState(readUrl);
      const [ready, setReady] = React.useState(false);
      const [generation, setGeneration] = React.useState(0);

      const reload = () => {
        setReady(false);
        setGeneration((value) => value + 1);
      };
      const save = () => {
        const next = draft.trim() || DEFAULT_URL;
        storeUrl(next);
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
          h('strong', { style: { fontSize: 12, letterSpacing: '.08em', textTransform: 'uppercase' } }, 'CEO cockpit'),
          h('input', {
            value: draft,
            onChange: (event) => setDraft(event.target.value),
            onKeyDown: (event) => {
              if (event.key === 'Enter') save();
            },
            'aria-label': 'Cockpit URL',
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
                color: ready ? 'var(--dsw-alias-state-success-primary)' : 'var(--dsw-alias-label-secondary)',
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
            title: 'SUSI CEO cockpit',
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
              'Start the CEO cockpit, then reload: ',
              h('br'),
              'http://localhost:8080/ (standalone app; susi\'s own panel is `susi harness cockpit --port 9181`)',
            ),
        ),
      );
    }

    return {
      inject: ['slots'],
      apply(ctx) {
        ctx.slots.inject('sidebar.panellist', () =>
          ctx.slots.register(
            {
              name: 'sidebar.panellist',
              id: 'susi-cockpit',
              order: 20,
              label: () => 'CEO cockpit',
            },
            Icon,
          ),
        );
        ctx.slots.inject('main', () =>
          ctx.slots.register(
            {
              name: 'main',
              key: 'susi-cockpit',
            },
            CockpitPanel,
          ),
        );
      },
    };
  },
});
