"""Generates the 2x body frames for assets/sprites.txt from parts (box, arms,
legs), so every pose shares the same shading, outline and proportions.
Run: python scripts/gen-bodies.py  → paste the frames you need into sprites.txt
(sprites.txt is the source of truth; generated frames may be hand-edited)."""

LEGS = (5, 9, 21, 25)


class F:
    def __init__(self, name, w=32, h=22, ox=0):
        self.name, self.w, self.h, self.ox = name, w, h, ox
        self.g = [['.'] * w for _ in range(h)]
        self.eyes = []

    def set(self, x, y, c):
        x += self.ox
        if 0 <= x < self.w and 0 <= y < self.h:
            self.g[y][x] = c

    def get(self, x, y):
        x += self.ox
        return self.g[y][x] if 0 <= x < self.w and 0 <= y < self.h else '.'

    def box(self, top, bot, x0=4, x1=27, eye_dy=5, blush=True):
        for x in range(x0 + 1, x1):
            self.set(x, top, 'R')
            self.set(x, bot, 'R')
        for y in range(top + 1, bot):
            self.set(x0, y, 'R')
            self.set(x1, y, 'R')
            for x in range(x0 + 1, x1):
                r = y - top
                if r == 1:
                    c = 'h' if x0 + 3 <= x <= x0 + 8 else 'L'
                elif r == 2:
                    c = 'L'
                elif y == bot - 1:
                    c = 'E'
                elif y == bot - 2:
                    c = 'D'
                else:
                    c = 'O'
                    if x == x0 + 1 and r <= 6:
                        c = 'L'
                    if x == x1 - 1:
                        c = 'D'
                self.set(x, y, c)
        ey = top + eye_dy
        self.eyes = [(x0 + 7 + self.ox, ey), (x1 - 6 + self.ox, ey)]  # centre lines of the 2-px eyes
        by = ey + 4
        if blush and by <= bot - 3:
            for x in (x0 + 4, x0 + 5, x1 - 5, x1 - 4):
                self.set(x, by, 'P')
        self.x0, self.x1, self.top, self.bot = x0, x1, top, bot

    # --- arms -----------------------------------------------------------
    def arm_side(self, side, ay):
        """A nub sticking out sideways: 5 rows (outline, L, O, D, outline)."""
        x0, x1 = self.x0, self.x1
        if side == 'l':
            for x in range(x0 - 3, x0):
                self.set(x, ay, 'R')
                self.set(x, ay + 4, 'R')
            for i, c in enumerate('LOD'):
                self.set(x0 - 4, ay + 1 + i, 'R')
                for x in range(x0 - 3, x0 + 1):
                    self.set(x, ay + 1 + i, c)
        else:
            for x in range(x1 + 1, x1 + 4):
                self.set(x, ay, 'R')
                self.set(x, ay + 4, 'R')
            for i, c in enumerate('LOD'):
                self.set(x1 + 4, ay + 1 + i, 'R')
                self.set(x1, ay + 1 + i, 'D')
                for x in range(x1 + 1, x1 + 4):
                    self.set(x, ay + 1 + i, c)

    def arm_up(self, side, y_top=0):
        """Raised straight up beside the head, joining at the shoulder."""
        top = self.top
        y_bot = top + 7  # bottom cap
        if side == 'l':
            xa, xb, edge = self.x0 - 4, self.x0, self.x0
            cols = ('L', 'O', 'O')
        else:
            xa, xb, edge = self.x1, self.x1 + 4, self.x1
            cols = ('O', 'O', 'D')
        for x in range(xa + 1, xb):
            self.set(x, y_top, 'R')
            self.set(x, y_bot, 'R')
        for y in range(y_top + 1, y_bot):
            self.set(xa, y, 'R')
            self.set(xb, y, 'R')
            for i, x in enumerate(range(xa + 1, xb)):
                c = 'L' if y == y_top + 1 else cols[i]
                self.set(x, y, c)
        # merge into the body at the shoulder
        for y in range(top + 3, top + 6):
            self.set(edge, y, 'O' if side == 'l' else 'D')

    def arm_tucked(self, side, ay):
        """Flat against the body (loaf / squash): 4 rows."""
        x0, x1 = self.x0, self.x1
        if side == 'l':
            self.set(x0 - 1, ay, 'R')
            self.set(x0 - 2, ay + 1, 'R'); self.set(x0 - 1, ay + 1, 'L')
            self.set(x0 - 2, ay + 2, 'R'); self.set(x0 - 1, ay + 2, 'D')
            self.set(x0 - 1, ay + 3, 'R')
            self.set(x0, ay + 1, 'L'); self.set(x0, ay + 2, 'D')
        else:
            self.set(x1 + 1, ay, 'R')
            self.set(x1 + 2, ay + 1, 'R'); self.set(x1 + 1, ay + 1, 'O')
            self.set(x1 + 2, ay + 2, 'R'); self.set(x1 + 1, ay + 2, 'D')
            self.set(x1 + 1, ay + 3, 'R')
            self.set(x1, ay + 1, 'D'); self.set(x1, ay + 2, 'D')

    # --- legs -----------------------------------------------------------
    def legs(self, y0, n, xs=LEGS, short=()):
        """n rows of D/E per leg, then a rust sole. Legs in `short` are lifted 1 row."""
        for x in xs:
            k = n - 1 if x in short else n
            for y in range(y0, y0 + k):
                self.set(x, y, 'D')
                self.set(x + 1, y, 'E')
            self.set(x, y0 + k, 'R')
            self.set(x + 1, y0 + k, 'R')

    def mouth(self, rows):
        for dy, row in enumerate(rows):
            for dx, c in enumerate(row):
                if c != '.':
                    self.set(16 - len(row) // 2 + dx, self.top + 10 + dy, c)

    def text(self, anchor, extra=''):
        ax, ay = anchor
        eyes = ';'.join(f'{x},{y}' for x, y in self.eyes)
        head = f'@frame {self.name} anchor={ax},{ay}' + (f' eyes={eyes}' if self.eyes else '') + extra
        return head + '\n' + '\n'.join(''.join(r) for r in self.g)


def stand(name='stand', short=()):
    f = F(name)
    f.box(2, 17)
    f.arm_side('l', 10)
    f.arm_side('r', 10)
    f.legs(18, 3, short=short)
    return f


frames = []

def add(f, anchor=None, comment=None):
    if comment:
        frames.append('// ' + comment)
    frames.append(f.text(anchor or (16 + f.ox, f.h - 1)))

add(stand(), comment='Standing. Eyes are drawn by the rig at the eyes= anchors.')

f = F('breathe'); f.box(3, 17); f.arm_side('l', 11); f.arm_side('r', 11); f.legs(18, 3)
add(f, comment='Breathing out: the top sinks 1 px, the arms with it.')

f = F('raised', h=22); f.box(2, 17); f.arm_up('l'); f.arm_up('r'); f.legs(18, 3)
add(f)

f = F('wave'); f.box(2, 17); f.arm_side('l', 10); f.arm_up('r'); f.legs(18, 3)
add(f)

f = F('wave_l'); f.box(2, 17); f.arm_up('l'); f.arm_side('r', 10); f.legs(18, 3)
add(f)

f = F('crouch', w=36, ox=2); f.box(6, 18, x0=3, x1=28, eye_dy=5); f.arm_side('l', 12); f.arm_side('r', 12)
f.legs(19, 2, xs=(4, 9, 21, 26))
add(f, comment='Anticipation / landing: 4 px shorter, 2 px wider, knees bent.')

f = F('squash', w=36, ox=2); f.box(10, 19, x0=2, x1=29, eye_dy=3, blush=False); f.arm_tucked('l', 13); f.arm_tucked('r', 13)
f.legs(20, 1, xs=(3, 9, 21, 27))
add(f, comment='Compacting, flattest.')

f = F('stretch', h=24); f.box(2, 19, x0=6, x1=25); f.arm_up('l'); f.arm_up('r'); f.legs(20, 3, xs=(7, 10, 20, 23))
add(f, comment='Wake-up stretch / spring-back overshoot: taller, narrower, arms up.')

def dangle(name, short):
    f = F(name, h=24)
    f.box(2, 17)
    f.arm_up('l')
    f.arm_up('r')
    f.legs(18, 5, short=short)
    return f


add(dangle('dangle_0', (9, 25)), comment='Picked up: arms up, legs dangling longer and kicking (two frames).')
add(dangle('dangle_1', (5, 21)))


def sheared(src, name, shift):
    """Swinging from the grab point: rows slide sideways more toward the
    bottom (shift px at the feet, negative = left). Keeps the anchor (the
    unswung feet point), so the top stays put while the legs swing."""
    pad = 5
    f = F(name, w=src.w + 2 * pad, h=src.h)
    for y, row in enumerate(src.g):
        off = round(shift * y / (src.h - 1))
        for x, c in enumerate(row):
            if c != '.':
                f.g[y][x + pad + off] = c
    f.eyes = [(x + pad + round(shift * y / (src.h - 1)), y) for x, y in src.eyes]
    return f, (16 + pad, src.h - 1)


for side, sign in (('l', -1), ('r', 1)):
    for level, shift in ((1, 2), (2, 4)):
        for k, short in ((0, (9, 25)), (1, (5, 21))):
            f, anchor = sheared(dangle('x', short), f'swing_{side}{level}_{k}', sign * shift)
            add(f, anchor, comment=None if (level, k) != (1, 0) else
                f'Swinging while dragged: legs trail {"left" if sign < 0 else "right"} (2 levels × 2 kick frames).')

def leaned(src, name, shift):
    """Riding a moving window: the feet stay planted and the top sways
    (shift px at the head, negative = left)."""
    pad = 5
    f = F(name, w=src.w + 2 * pad, h=src.h)
    last = src.h - 1
    for y, row in enumerate(src.g):
        off = round(shift * (last - y) / last)
        for x, c in enumerate(row):
            if c != '.':
                f.g[y][x + pad + off] = c
    f.eyes = [(x + pad + round(shift * (last - y) / last), y) for x, y in src.eyes]
    return f, (16 + pad, last)


for side, sign in (('l', -1), ('r', 1)):
    for level, shift in ((1, 2), (2, 4)):
        f, anchor = leaned(stand(), f'ride_{side}{level}', sign * shift)
        add(f, anchor, comment=None if level != 1 else
            f'Riding a window that moves: feet planted, top swaying {"left" if sign < 0 else "right"} (2 levels).')

# Moving fast the body stretches along the motion. Going up the arms pin to
# the sides, coming down they stream above the head and the legs flutter.
f = F('rise_1', h=24); f.box(2, 19, x0=6, x1=25); f.arm_tucked('l', 9); f.arm_tucked('r', 9)
f.legs(20, 3, xs=(7, 10, 20, 23))
add(f, comment='Rising: arms pinned, body long (faster = longer).')

f = F('rise_2', h=26); f.box(2, 21, x0=7, x1=24); f.arm_tucked('l', 9); f.arm_tucked('r', 9)
f.legs(22, 3, xs=(8, 11, 19, 22))
f.eyes = [(13, 7), (19, 7)]  # the default anchors crowd together this narrow
add(f)

for k, short in ((0, (10, 23)), (1, (7, 20))):
    f = F(f'fall_1_{k}', h=24); f.box(2, 19, x0=6, x1=25); f.arm_up('l'); f.arm_up('r')
    f.legs(20, 3, xs=(7, 10, 20, 23), short=short)
    add(f, comment=None if k else 'Falling: arms streaming up, legs fluttering (faster = longer).')

for k, short in ((0, (11, 22)), (1, (8, 19))):
    f = F(f'fall_2_{k}', h=26); f.box(2, 21, x0=7, x1=24); f.arm_up('l'); f.arm_up('r')
    f.legs(22, 3, xs=(8, 11, 19, 22), short=short)
    f.eyes = [(13, 7), (19, 7)]
    add(f)

f = F('splat', w=40, ox=4); f.box(13, 20, x0=0, x1=31, eye_dy=3, blush=False)
f.arm_tucked('l', 15); f.arm_tucked('r', 15); f.legs(21, 0, xs=(2, 8, 22, 28))
add(f, comment='Dropped from high up: a pancake, wider than anything else.')

f = F('air', h=20); f.box(2, 17); f.arm_up('l'); f.arm_up('r'); f.legs(18, 1)
add(f, comment='Airborne: arms up, legs tucked.')

f = F('stretch_2', h=26); f.box(2, 21, x0=7, x1=24); f.arm_up('l'); f.arm_up('r'); f.legs(22, 3, xs=(8, 11, 19, 22))
add(f, comment='Spring-back overshoot: tallest, narrowest.')

f = F('cling', h=24); f.box(2, 17); f.arm_up('l'); f.arm_side('r', 10); f.legs(18, 5, short=(9, 25))
add(f, comment='Hanging on the left screen edge (flipped for the right edge).')

f = F('side'); f.box(2, 17, x0=10, x1=21, blush=False); f.eyes = []; f.legs(18, 3, xs=(12, 18))
add(f, comment='Turned sideways mid-spin (no eyes).')

f = F('sit'); f.box(4, 19); f.arm_side('l', 12); f.arm_side('r', 12); f.legs(20, 1, xs=(3, 8, 22, 27))
add(f)

f = F('sit_breathe'); f.box(5, 19); f.arm_side('l', 13); f.arm_side('r', 13); f.legs(20, 1, xs=(3, 8, 22, 27))
add(f)

f = F('loaf'); f.box(8, 20, x0=3, x1=28, eye_dy=5); f.arm_tucked('l', 14); f.arm_tucked('r', 14)
f.legs(21, 0, xs=(6, 10, 20, 24))
add(f, comment='Asleep: lying down, legs tucked in.')

f = F('loaf_breathe'); f.box(9, 20, x0=3, x1=28, eye_dy=4); f.arm_tucked('l', 14); f.arm_tucked('r', 14)
f.legs(21, 0, xs=(6, 10, 20, 24))
add(f)

f = stand('yawn_s'); f.mouth(['KK', 'KK'])
add(f)

f = stand('yawn'); f.mouth(['.KK.', 'KHHK', 'KHHK', '.KK.'])
add(f)

f = F('type_0'); f.box(2, 17); f.arm_side('l', 10); f.arm_side('r', 12); f.legs(18, 3)
add(f, comment='Typing / tapping: one arm down, the other up.')

f = F('type_1'); f.box(2, 17); f.arm_side('l', 12); f.arm_side('r', 10); f.legs(18, 3)
add(f)

add(stand('walk_0', short=(5, 21)), comment='Walk cycle (Phase 5): legs lift in pairs.')
add(stand('walk_2', short=(9, 25)))

f = F('tap'); f.box(2, 17); f.arm_side('l', 10); f.arm_side('r', 10)
f.legs(18, 3, xs=(5, 9, 21)); f.legs(18, 1, xs=(25,))
add(f, comment='Impatient (Phase 5): the outer right foot lifted, tapping.')

print('\n\n'.join(frames))
