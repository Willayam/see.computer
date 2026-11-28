// Shared motion for the family of sites: the header cap, pressed keys, the
// hero entrance, scroll reveals and the ember under the current nav link.
(() => {
  const root = document.documentElement;
  const still = matchMedia('(prefers-reduced-motion: reduce)').matches;

  const onScroll = () => root.classList.toggle('scrolled', scrollY > 8);
  addEventListener('scroll', onScroll, { passive: true });
  onScroll();

  document.addEventListener('pointerdown', (event) => {
    const key = event.target.closest?.('.key');
    if (!key) return;
    key.classList.add('down');
    const up = () => {
      key.classList.remove('down');
      for (const type of ['pointerup', 'pointercancel', 'pointerleave']) key.removeEventListener(type, up);
    };
    for (const type of ['pointerup', 'pointercancel', 'pointerleave']) key.addEventListener(type, up);
  });

  const hero = document.querySelector('.hero');
  if (hero && !still && document.fonts?.load && 'PerformancePaintTiming' in window
      && !performance.getEntriesByName('first-contentful-paint').length) {
    const enter = () => hero.classList.add('in');
    setTimeout(enter, 1000);
    root.classList.add('js');
    Promise.all([document.fonts.load('600 60px Fraunces'), document.fonts.load('italic 500 60px Fraunces')]).then(enter, enter);
  }

  if (!still && 'IntersectionObserver' in window) {
    const arrivals = new IntersectionObserver((entries) => {
      let order = 0;
      for (const entry of entries) {
        if (!entry.isIntersecting) continue;
        const el = entry.target;
        arrivals.unobserve(el);
        const step = Math.min(order++, 5);
        el.style.setProperty('--rv-i', step);
        el.classList.add('in');
        setTimeout(() => {
          el.removeAttribute('data-rv');
          el.classList.remove('in');
          el.style.removeProperty('--rv-i');
        }, 1100 + step * 80);
      }
    }, { rootMargin: '0px 0px -7% 0px', threshold: 0.04 });
    for (const el of document.querySelectorAll('[data-reveal]')) {
      if (el.getBoundingClientRect().top < innerHeight) continue;
      el.setAttribute('data-rv', '');
      arrivals.observe(el);
    }
  }

  const nav = document.querySelector('.nav');
  const links = nav ? [...nav.querySelectorAll('a[href^="#"]')] : [];
  if (links.length) {
    const sections = links.map((link) => document.querySelector(link.getAttribute('href')));
    const mark = () => {
      const line = innerHeight * 0.4;
      let current = -1;
      sections.forEach((section, i) => {
        if (section && section.getBoundingClientRect().top <= line) current = i;
      });
      links.forEach((link, i) => link.classList.toggle('on', i === current));
      if (current < 0) return nav.removeAttribute('data-on');
      const link = links[current];
      nav.style.setProperty('--x', `${link.offsetLeft + link.offsetWidth / 2}px`);
      nav.setAttribute('data-on', '');
    };
    addEventListener('scroll', mark, { passive: true });
    addEventListener('resize', mark);
    mark();
  }
})();
