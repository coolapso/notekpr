(() => {
  const toggle = document.querySelector('.nav-toggle');
  const sidebar = document.querySelector('.sidebar');
  if (!toggle || !sidebar) return;
  function close() { sidebar.classList.remove('open'); toggle.setAttribute('aria-expanded', 'false'); }
  toggle.addEventListener('click', () => {
    toggle.setAttribute('aria-expanded', String(sidebar.classList.toggle('open')));
  });
  document.addEventListener('keydown', event => { if (event.key === 'Escape' && sidebar.classList.contains('open')) { close(); toggle.focus(); } });
  sidebar.addEventListener('click', event => { if (event.target.closest('a')) close(); });
})();

// A copy button on every code block, when the site turns it on (params.copyCode).
(() => {
  const root = document.documentElement;
  const label = root.dataset.copy;
  if (!label || !navigator.clipboard) return;
  for (const block of document.querySelectorAll('.doc .highlight')) {
    const code = block.querySelector('.lntd:last-child code, td:last-child code') || block.querySelector('code');
    if (!code) continue;
    const button = document.createElement('button');
    button.type = 'button';
    button.className = 'copy-code';
    button.textContent = label;
    button.addEventListener('click', async () => {
      try {
        await navigator.clipboard.writeText(code.innerText.replace(/\n$/, ''));
        button.textContent = root.dataset.copied;
      } catch {
        return;
      }
      setTimeout(() => { button.textContent = label; }, 1500);
    });
    block.append(button);
  }
})();
