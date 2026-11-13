//! MNA matrix + LU solve, generic over real (DC/transient) and complex (AC).

use std::collections::{BTreeSet, HashMap};
use std::ops::{Add, AddAssign, Div, Mul, Neg, Sub, SubAssign};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct C64 {
    pub re: f64,
    pub im: f64,
}

impl C64 {
    pub const fn new(re: f64, im: f64) -> Self {
        C64 { re, im }
    }
    pub fn polar(mag: f64, deg: f64) -> Self {
        let r = deg.to_radians();
        C64::new(mag * r.cos(), mag * r.sin())
    }
    pub fn norm(self) -> f64 {
        self.re.hypot(self.im)
    }
    pub fn arg_deg(self) -> f64 {
        self.im.atan2(self.re).to_degrees()
    }
}
impl Add for C64 {
    type Output = C64;
    fn add(self, o: C64) -> C64 {
        C64::new(self.re + o.re, self.im + o.im)
    }
}
impl Sub for C64 {
    type Output = C64;
    fn sub(self, o: C64) -> C64 {
        C64::new(self.re - o.re, self.im - o.im)
    }
}
impl Mul for C64 {
    type Output = C64;
    fn mul(self, o: C64) -> C64 {
        C64::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}
impl Div for C64 {
    type Output = C64;
    fn div(self, o: C64) -> C64 {
        let d = o.re * o.re + o.im * o.im;
        C64::new(
            (self.re * o.re + self.im * o.im) / d,
            (self.im * o.re - self.re * o.im) / d,
        )
    }
}
impl Neg for C64 {
    type Output = C64;
    fn neg(self) -> C64 {
        C64::new(-self.re, -self.im)
    }
}
impl AddAssign for C64 {
    fn add_assign(&mut self, o: C64) {
        *self = *self + o;
    }
}
impl SubAssign for C64 {
    fn sub_assign(&mut self, o: C64) {
        *self = *self - o;
    }
}
impl From<f64> for C64 {
    fn from(v: f64) -> C64 {
        C64::new(v, 0.0)
    }
}

pub trait Scalar:
    Copy
    + Default
    + PartialEq
    + Add<Output = Self>
    + Sub<Output = Self>
    + Mul<Output = Self>
    + Div<Output = Self>
    + Neg<Output = Self>
    + AddAssign
    + SubAssign
    + From<f64>
{
    fn mag(self) -> f64;
    fn finite(self) -> bool;
}
impl Scalar for f64 {
    fn mag(self) -> f64 {
        self.abs()
    }
    fn finite(self) -> bool {
        self.is_finite()
    }
}
impl Scalar for C64 {
    fn mag(self) -> f64 {
        self.re.abs() + self.im.abs()
    }
    fn finite(self) -> bool {
        self.re.is_finite() && self.im.is_finite()
    }
}

const NONE: u32 = u32::MAX;
/// Threshold partial pivoting: keep the diagonal while it is within this factor of the column max.
const PIVOT_TOL: f64 = 1e-3;
/// Keep the old pivot order while every pivot stays within this factor of its column
/// (looser than PIVOT_TOL so borderline diagonals don't re-pivot every solve).
const REUSE_TOL: f64 = 1e-4;
/// Pivot growth (max |U| / max |A|) beyond which threshold pivoting is distrusted.
const GROWTH_MAX: f64 = 1e8;
/// Up to this many unknowns the system stays a dense matrix with a dense LU, which
/// beats sparse bookkeeping on tiny circuits (crossover ~30-50 unknowns).
const DENSE_MAX: usize = 32;
/// Up to this many unknowns, values sit in a dense n×n array (at most 2 MB) even
/// when the LU is sparse; above it they are stored per entry behind a stamp-order cache.
const DENSE_IDX: usize = 512;
/// New table-mode positions remembered per load before falling back to a full scan.
const PEND: usize = 32;

/// Square system of `n` unknowns. Index 0 is ground and is dropped by the
/// stamping helpers, so devices index unknowns 1..=n directly.
///
/// Above DENSE_MAX unknowns it is sparse: the pattern is learned from the stamps and
/// only grows. Per pattern, a fill-reducing column order (minimum degree on A+Aᵀ) is
/// computed once; row pivots are picked by a left-looking (Gilbert-Peierls) LU with
/// threshold partial pivoting; later solves only redo the numeric LU on that fixed
/// L/U pattern, re-pivoting when a pivot gets too small or the growth too large.
///
/// Stamping stays call-free up to DENSE_IDX unknowns (new positions are only marked
/// and registered in `clear`/`solve`), and the large-n path calls only into the boxed
/// `Sparse`: a call that may touch these fields makes every device load reload them,
/// which cost tiny circuits ~10% against a plain dense matrix.
pub struct Matrix<T> {
    pub n: usize,
    b: Vec<T>,
    a: Vec<T>,      // n <= DENSE_IDX: dense row-major values
    nz: Vec<usize>, // dense LU scratch
    // table mode (DENSE_MAX < n <= DENSE_IDX): 0 = unseen, 1 = registered, 2 = new
    known: Vec<u8>,
    pend: [u32; PEND],
    np: usize,
    s: Box<Sparse<T>>,
}

/// Sparse pattern and LU, used above DENSE_MAX unknowns.
struct Sparse<T> {
    n: usize,
    // entry e sits at 0-based (row, col); its value is at slot[e] in the value array
    // (Matrix::a in table mode, `val` with slot[e] = e above DENSE_IDX)
    ent: Vec<(u32, u32)>,
    slot: Vec<u32>,
    val: Vec<T>,
    map: HashMap<(u32, u32), u32>,
    // stamp-order cache: the k-th add() since clear() usually hits seq[k] = (row, col, entry)
    seq: Vec<(u32, u32, u32)>,
    pos: usize,
    ordered: bool,
    pivoted: bool,
    tol: f64,       // diagonal-preference threshold for the current order
    q: Vec<u32>,    // step -> column
    pinv: Vec<u32>, // row -> step
    // A grouped by step column as (value slot, row step); L (unit diag) and U by step column
    ap: Vec<usize>,
    ae: Vec<(u32, u32)>,
    lp: Vec<usize>,
    li: Vec<u32>,
    lx: Vec<T>,
    up: Vec<usize>,
    ui: Vec<u32>,
    ux: Vec<T>,
    ud: Vec<T>,
    w: Vec<T>, // dense work vector, kept all-zero between uses
}

impl<T: Scalar> Matrix<T> {
    pub fn new(n: usize) -> Self {
        let table = n > DENSE_MAX && n <= DENSE_IDX;
        Matrix {
            n,
            b: vec![T::default(); n],
            a: if n <= DENSE_IDX {
                vec![T::default(); n * n]
            } else {
                Vec::new()
            },
            nz: Vec::new(),
            known: if table { vec![0; n * n] } else { Vec::new() },
            pend: [0; PEND],
            np: 0,
            s: Box::new(Sparse::new(n)),
        }
    }
    pub fn clear(&mut self) {
        if self.n <= DENSE_MAX {
            self.a.fill(T::default());
        } else if self.n <= DENSE_IDX {
            self.register();
            for &k in &self.s.slot {
                self.a[k as usize] = T::default();
            }
        } else {
            self.s.val.fill(T::default());
            self.s.pos = 0;
        }
        self.b.fill(T::default());
    }
    #[inline(always)]
    pub fn add(&mut self, r: usize, c: usize, v: T) {
        self.stamp([(r, c, v)]);
    }
    /// Stamps a group under one storage-mode check.
    #[inline(always)]
    fn stamp<const K: usize>(&mut self, es: [(usize, usize, T); K]) {
        if self.n <= DENSE_MAX {
            for (r, c, v) in es {
                if r != 0 && c != 0 {
                    self.a[(r - 1) * self.n + c - 1] += v;
                }
            }
        } else if self.n <= DENSE_IDX {
            for (r, c, v) in es {
                if r != 0 && c != 0 {
                    let k = (r - 1) * self.n + c - 1;
                    self.a[k] += v;
                    if self.known[k] == 0 {
                        self.known[k] = 2;
                        if self.np < PEND {
                            self.pend[self.np] = k as u32;
                        }
                        self.np += 1;
                    }
                }
            }
        } else {
            for (r, c, v) in es {
                self.s.add(r, c, v);
            }
        }
    }
    /// Table mode: turn positions first stamped since the last call into entries.
    fn register(&mut self) {
        if self.np == 0 {
            return;
        }
        let n = self.n;
        let ks: Vec<usize> = if self.np <= PEND {
            self.pend[..self.np].iter().map(|&k| k as usize).collect()
        } else {
            (0..n * n).filter(|&k| self.known[k] == 2).collect()
        };
        for k in ks {
            self.known[k] = 1;
            self.s.ent.push(((k / n) as u32, (k % n) as u32));
            self.s.slot.push(k as u32);
        }
        self.np = 0;
        self.s.ordered = false;
    }
    #[inline]
    pub fn rhs(&mut self, r: usize, v: T) {
        if r != 0 {
            self.b[r - 1] += v;
        }
    }
    /// Admittance `g` between nodes a and b.
    #[inline]
    pub fn g(&mut self, a: usize, b: usize, g: T) {
        self.stamp([(a, a, g), (b, b, g), (a, b, -g), (b, a, -g)]);
    }
    /// Constant current `i` flowing from a to b through the element.
    #[inline]
    pub fn i(&mut self, a: usize, b: usize, i: T) {
        self.rhs(a, -i);
        self.rhs(b, i);
    }
    /// Current g*(V(p)-V(n)) flowing from a to b through the element.
    #[inline]
    pub fn vccs(&mut self, a: usize, b: usize, p: usize, n: usize, g: T) {
        self.stamp([(a, p, g), (a, n, -g), (b, p, -g), (b, n, g)]);
    }
    /// Branch unknown `k` carries current from a to b (voltage-source style).
    #[inline]
    pub fn branch(&mut self, a: usize, b: usize, k: usize) {
        let (one, m1) = (T::from(1.0), T::from(-1.0));
        self.stamp([(a, k, one), (b, k, m1), (k, a, one), (k, b, m1)]);
    }

    /// Clear `t` and load the transpose of this matrix into it, with an empty
    /// right-hand side (adjoint solves: one Aᵀ·y = c gives c·A⁻¹·b for every b).
    pub fn transpose_into(&self, t: &mut Matrix<T>) {
        t.clear();
        let n = self.n;
        if n <= DENSE_IDX {
            for (k, &v) in self.a.iter().enumerate() {
                if v != T::default() {
                    t.add(k % n + 1, k / n + 1, v);
                }
            }
        } else {
            for (&(r, c), &v) in self.s.ent.iter().zip(&self.s.val) {
                t.add(c as usize + 1, r as usize + 1, v);
            }
        }
    }

    /// Solve; `x` has length n+1 and x[0] stays 0. Call `clear` and re-stamp before the next solve.
    /// On a singular matrix returns the unknown index that has no pivot.
    pub fn solve(&mut self, x: &mut [T]) -> Result<(), usize> {
        if self.n <= DENSE_MAX {
            self.solve_dense(x)
        } else if self.n <= DENSE_IDX {
            self.register();
            self.s.solve(&self.a, &self.b, x)
        } else {
            self.s.solve(&[], &self.b, x)
        }
    }

    /// Dense LU with partial pivoting, in place (skips zeros in the pivot row).
    fn solve_dense(&mut self, x: &mut [T]) -> Result<(), usize> {
        let n = self.n;
        let a = &mut self.a;
        let b = &mut self.b;
        for k in 0..n {
            let mut p = k;
            let mut best = a[k * n + k].mag();
            for i in k + 1..n {
                let m = a[i * n + k].mag();
                if m > best {
                    best = m;
                    p = i;
                }
            }
            if !(best > 1e-300) || !best.is_finite() {
                return Err(k + 1);
            }
            if p != k {
                for j in k..n {
                    a.swap(k * n + j, p * n + j);
                }
                b.swap(k, p);
            }
            self.nz.clear();
            for j in k + 1..n {
                if a[k * n + j] != T::default() {
                    self.nz.push(j);
                }
            }
            let piv = a[k * n + k];
            let bk = b[k];
            for i in k + 1..n {
                let f = a[i * n + k];
                if f == T::default() {
                    continue;
                }
                let f = f / piv;
                a[i * n + k] = T::default();
                for &j in &self.nz {
                    let v = a[k * n + j];
                    a[i * n + j] -= f * v;
                }
                b[i] -= f * bk;
            }
        }
        for k in (0..n).rev() {
            let mut s = b[k];
            for j in k + 1..n {
                let v = a[k * n + j];
                if v != T::default() {
                    s -= v * x[j + 1];
                }
            }
            x[k + 1] = s / a[k * n + k];
        }
        x[0] = T::default();
        Ok(())
    }
}

impl<T: Scalar> Sparse<T> {
    fn new(n: usize) -> Self {
        Sparse {
            n,
            ent: Vec::new(),
            slot: Vec::new(),
            val: Vec::new(),
            map: HashMap::new(),
            seq: Vec::new(),
            pos: 0,
            ordered: false,
            pivoted: false,
            tol: PIVOT_TOL,
            q: Vec::new(),
            pinv: Vec::new(),
            ap: Vec::new(),
            ae: Vec::new(),
            lp: Vec::new(),
            li: Vec::new(),
            lx: Vec::new(),
            up: Vec::new(),
            ui: Vec::new(),
            ux: Vec::new(),
            ud: Vec::new(),
            w: if n > DENSE_MAX {
                vec![T::default(); n]
            } else {
                Vec::new()
            },
        }
    }
    /// Large-n stamp through the stamp-order cache.
    #[inline(always)]
    fn add(&mut self, r: usize, c: usize, v: T) {
        // ground stamps still take a slot so the cache stays aligned when a device's
        // terminals swap (e.g. MOSFET drain/source) and change which stamps hit ground
        let k = self.pos;
        self.pos += 1;
        if r == 0 || c == 0 {
            return;
        }
        let (r, c) = (r as u32 - 1, c as u32 - 1);
        let e = match self.seq.get(k) {
            Some(&(sr, sc, e)) if sr == r && sc == c => e as usize,
            _ => self.miss(k, (r, c)),
        };
        self.val[e] += v;
    }
    #[cold]
    fn miss(&mut self, k: usize, key: (u32, u32)) -> usize {
        let e = match self.map.get(&key) {
            Some(&e) => e,
            None => self.insert(key),
        };
        if k >= self.seq.len() {
            self.seq.resize(k + 1, (NONE, NONE, NONE));
        }
        self.seq[k] = (key.0, key.1, e);
        e as usize
    }
    #[cold]
    fn insert(&mut self, key: (u32, u32)) -> u32 {
        let e = self.ent.len() as u32;
        self.map.insert(key, e);
        self.ent.push(key);
        self.slot.push(e);
        self.val.push(T::default());
        self.ordered = false;
        e
    }

    /// `ext` holds the values (indexed by `slot`) in table mode; empty means `val`.
    fn solve(&mut self, ext: &[T], b: &[T], x: &mut [T]) -> Result<(), usize> {
        if !self.ordered {
            self.order();
            self.ordered = true;
            self.pivoted = false;
            self.tol = PIVOT_TOL;
        }
        if !(self.pivoted && self.refactor(ext))
            && self.factor(ext, self.tol)? > GROWTH_MAX
            && self.tol < 1.0
        {
            // chains of gain stages (e.g. a ring oscillator at DC) compound the growth
            // that threshold pivoting allows: fall back to strict partial pivoting
            self.tol = 1.0;
            self.factor(ext, 1.0)?;
        }
        let Sparse {
            q,
            pinv,
            lp,
            li,
            lx,
            up,
            ui,
            ux,
            ud,
            w,
            ..
        } = self;
        for (&p, &v) in pinv.iter().zip(b.iter()) {
            w[p as usize] = v;
        }
        for (k, r) in lp.windows(2).enumerate() {
            let wk = w[k];
            if wk != T::default() {
                for (&i, &l) in li[r[0]..r[1]].iter().zip(&lx[r[0]..r[1]]) {
                    w[i as usize] -= l * wk;
                }
            }
        }
        for (k, r) in up.windows(2).enumerate().rev() {
            let wk = w[k] / ud[k];
            w[k] = T::default();
            x[q[k] as usize + 1] = wk;
            if wk != T::default() {
                for (&j, &u) in ui[r[0]..r[1]].iter().zip(&ux[r[0]..r[1]]) {
                    w[j as usize] -= u * wk;
                }
            }
        }
        x[0] = T::default();
        Ok(())
    }

    /// Fill-reducing column order: minimum degree on the graph of A+Aᵀ.
    fn order(&mut self) {
        let n = self.n;
        let mut adj = vec![Vec::<u32>::new(); n];
        for &(r, c) in &self.ent {
            if r != c {
                adj[r as usize].push(c);
                adj[c as usize].push(r);
            }
        }
        for a in adj.iter_mut() {
            a.sort_unstable();
            a.dedup();
        }
        let mut heap: BTreeSet<(usize, u32)> = (0..n).map(|i| (adj[i].len(), i as u32)).collect();
        let mut tmp = Vec::new();
        self.q.clear();
        while let Some((_, v)) = heap.pop_first() {
            self.q.push(v);
            // eliminating v joins its live neighbours into a clique
            let nb = std::mem::take(&mut adj[v as usize]);
            for &u in &nb {
                let a = &mut adj[u as usize];
                heap.remove(&(a.len(), u));
                tmp.clear();
                let (mut i, mut j) = (0, 0);
                while i < a.len() || j < nb.len() {
                    let y = if j == nb.len() || (i < a.len() && a[i] <= nb[j]) {
                        if j < nb.len() && nb[j] == a[i] {
                            j += 1;
                        }
                        i += 1;
                        a[i - 1]
                    } else {
                        j += 1;
                        nb[j - 1]
                    };
                    if y != u && y != v {
                        tmp.push(y);
                    }
                }
                std::mem::swap(a, &mut tmp);
                heap.insert((a.len(), u));
            }
        }
    }

    /// Left-looking LU with threshold partial pivoting along the column order `q`;
    /// fixes the row pivots and the L/U pattern for `refactor`. Returns the pivot growth.
    fn factor(&mut self, ext: &[T], tol: f64) -> Result<f64, usize> {
        let n = self.n;
        self.pivoted = false;
        let mut cp = vec![0usize; n + 1];
        for &(_, c) in &self.ent {
            cp[c as usize + 1] += 1;
        }
        for i in 0..n {
            cp[i + 1] += cp[i];
        }
        let mut ce = vec![0u32; self.ent.len()];
        let mut next = cp.clone();
        for (e, &(_, c)) in self.ent.iter().enumerate() {
            ce[next[c as usize]] = e as u32;
            next[c as usize] += 1;
        }
        let Sparse {
            ent,
            slot,
            val,
            q,
            pinv,
            ap,
            ae,
            lp,
            li,
            lx,
            up,
            ui,
            ux,
            ud,
            w: x,
            ..
        } = self;
        let val: &[T] = if ext.is_empty() { val } else { ext };
        pinv.clear();
        pinv.resize(n, NONE);
        for v in [&mut *ap, &mut *lp, &mut *up] {
            v.clear();
            v.push(0);
        }
        ae.clear();
        li.clear();
        lx.clear();
        ui.clear();
        ux.clear();
        ud.clear();
        let (mut amax, mut umax) = (0.0f64, 0.0f64);
        let mut mark = vec![usize::MAX; n];
        let (mut xi, mut stack, mut pstack) = (vec![0u32; n], vec![0u32; n], vec![0usize; n]);
        for k in 0..n {
            let col = q[k] as usize;
            let acol = &ce[cp[col]..cp[col + 1]];
            // pattern of column k: rows reachable from A(:,col) through L, topologically ordered
            let mut top = n;
            for &e in acol {
                let r = ent[e as usize].0;
                if mark[r as usize] == k {
                    continue;
                }
                mark[r as usize] = k;
                let start = |i: u32| {
                    if pinv[i as usize] == NONE {
                        0
                    } else {
                        lp[pinv[i as usize] as usize]
                    }
                };
                stack[0] = r;
                pstack[0] = start(r);
                let mut head = 1;
                while head > 0 {
                    let j = stack[head - 1];
                    let end = if pinv[j as usize] == NONE {
                        0
                    } else {
                        lp[pinv[j as usize] as usize + 1]
                    };
                    let mut p = pstack[head - 1];
                    while p < end && mark[li[p] as usize] == k {
                        p += 1;
                    }
                    if p < end {
                        let i = li[p];
                        mark[i as usize] = k;
                        pstack[head - 1] = p + 1;
                        stack[head] = i;
                        pstack[head] = start(i);
                        head += 1;
                    } else {
                        head -= 1;
                        top -= 1;
                        xi[top] = j;
                    }
                }
            }
            let pat = &xi[top..n];
            for &e in acol {
                let v = val[slot[e as usize] as usize];
                x[ent[e as usize].0 as usize] = v;
                amax = amax.max(v.mag());
            }
            for &i in pat {
                let j = pinv[i as usize];
                if j != NONE {
                    let xj = x[i as usize];
                    for p in lp[j as usize]..lp[j as usize + 1] {
                        x[li[p] as usize] -= lx[p] * xj;
                    }
                }
            }
            let (mut piv, mut best) = (usize::MAX, -1.0);
            for &i in pat {
                let m = x[i as usize].mag();
                umax = umax.max(m);
                if pinv[i as usize] == NONE && m > best {
                    (piv, best) = (i as usize, m);
                }
            }
            if pinv[col] == NONE && mark[col] == k && x[col].mag() >= tol * best {
                piv = col;
            }
            if piv == usize::MAX || !(x[piv].mag() > 1e-300) || !x[piv].finite() {
                x.fill(T::default());
                // overflow under threshold pivoting is growth, not singularity
                return if tol < 1.0 && !umax.is_finite() {
                    Ok(f64::INFINITY)
                } else {
                    Err(col + 1)
                };
            }
            let pv = x[piv];
            pinv[piv] = k as u32;
            ud.push(pv);
            for &i in pat {
                let j = pinv[i as usize];
                if j < k as u32 {
                    ui.push(j);
                    ux.push(x[i as usize]);
                } else if i as usize != piv {
                    li.push(i);
                    lx.push(x[i as usize] / pv);
                }
                x[i as usize] = T::default();
            }
            lp.push(li.len());
            up.push(ui.len());
        }
        for i in li.iter_mut() {
            *i = pinv[*i as usize];
        }
        for &c in q.iter() {
            for &e in &ce[cp[c as usize]..cp[c as usize + 1]] {
                ae.push((slot[e as usize], pinv[ent[e as usize].0 as usize]));
            }
            ap.push(ae.len());
        }
        self.pivoted = true;
        Ok(umax / amax)
    }

    /// Numeric LU on the fixed pattern and pivots; false if a pivot got too small
    /// or the growth too large.
    fn refactor(&mut self, ext: &[T]) -> bool {
        let Sparse {
            n,
            val,
            ap,
            ae,
            lp,
            li,
            lx,
            up,
            ui,
            ux,
            ud,
            w: x,
            ..
        } = self;
        let val: &[T] = if ext.is_empty() { val } else { ext };
        // plain `>` maxima: f64::max's NaN handling is measurably slower in these loops
        let (mut amax, mut umax, mut lmax) = (0.0f64, 0.0f64, 0.0f64);
        for k in 0..*n {
            for &(e, s) in &ae[ap[k]..ap[k + 1]] {
                let v = val[e as usize];
                x[s as usize] = v;
                if v.mag() > amax {
                    amax = v.mag();
                }
            }
            for p in up[k]..up[k + 1] {
                let j = ui[p] as usize;
                let xj = x[j];
                x[j] = T::default();
                ux[p] = xj;
                if xj.mag() > umax {
                    umax = xj.mag();
                }
                if xj != T::default() {
                    let r = lp[j]..lp[j + 1];
                    for (&i, &l) in li[r.clone()].iter().zip(&lx[r]) {
                        x[i as usize] -= l * xj;
                    }
                }
            }
            let pv = x[k];
            x[k] = T::default();
            if !(pv.mag() > 1e-300) {
                x.fill(T::default());
                return false;
            }
            if pv.mag() > umax {
                umax = pv.mag();
            }
            ud[k] = pv;
            let r = lp[k]..lp[k + 1];
            for (&i, l) in li[r.clone()].iter().zip(&mut lx[r]) {
                *l = x[i as usize] / pv;
                x[i as usize] = T::default();
                if l.mag() > lmax {
                    lmax = l.mag();
                }
            }
        }
        // a pivot below REUSE_TOL of its column shows up as a multiplier above 1/REUSE_TOL;
        // an infinite pivot or U entry fails the growth test
        lmax <= 1.0 / REUSE_TOL && umax <= GROWTH_MAX * amax
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn solves_divider_with_pivoting() {
        // V source (branch 3) at node 1 = 10V, R 1k from 1-2, R 1k from 2-0
        let mut m = Matrix::<f64>::new(3);
        m.g(1, 2, 1e-3);
        m.g(2, 0, 1e-3);
        m.branch(1, 0, 3);
        m.rhs(3, 10.0);
        let mut x = vec![0.0; 4];
        m.solve(&mut x).unwrap();
        assert!((x[1] - 10.0).abs() < 1e-12);
        assert!((x[2] - 5.0).abs() < 1e-12);
        assert!((x[3] + 5e-3).abs() < 1e-15);
    }

    #[test]
    fn all_storage_modes_solve_with_small_residual() {
        // resistor chain + grounded and floating sources + a 2x2 block whose diagonal
        // pivot vanishes on odd solves (forces a re-pivot) + a resistor that appears on
        // the 4th solve (pattern growth after factoring); sizes hit dense, table, cache
        for n in [20, 60, 600] {
            let mut m = Matrix::<f64>::new(n);
            let mut x = vec![0.0; n + 1];
            let c = n - 4; // chain nodes 1..=c, branches c+1, c+2, block c+3, c+4
            for it in 0..6 {
                let g = 1e-3 * (it + 1) as f64;
                let mut a = vec![(c, c, 1e-3)];
                for i in 1..c {
                    a.extend([(i, i, g), (i + 1, i + 1, g), (i, i + 1, -g), (i + 1, i, -g)]);
                }
                for (k, p, q) in [(c + 1, 1, 0), (c + 2, c / 2, c - 1)] {
                    a.extend([(p, k, 1.0), (q, k, -1.0), (k, p, 1.0), (k, q, -1.0)]);
                }
                if it >= 3 {
                    a.extend([(1, 1, 1e-3), (c, c, 1e-3), (1, c, -1e-3), (c, 1, -1e-3)]);
                }
                let d = if it % 2 == 0 { 1.0 } else { 0.0 };
                a.extend([(c + 3, c + 3, d), (c + 3, c + 4, 1.0), (c + 4, c + 3, 1.0)]);
                let mut b = vec![0.0; n + 1];
                (b[c + 1], b[c + 2], b[c + 3], b[c + 4]) = (1.0, 0.5, 2.0, 3.0);
                m.clear();
                for &(i, j, v) in &a {
                    m.add(i, j, v);
                }
                for (i, &v) in b.iter().enumerate() {
                    m.rhs(i, v);
                }
                m.solve(&mut x).unwrap();
                for &(i, j, v) in &a {
                    b[i] -= v * x[j];
                }
                assert!(
                    b[1..].iter().all(|r| r.abs() < 1e-12),
                    "n={n} it={it}: {:?}",
                    &b[1..]
                );
            }
        }
    }

    #[test]
    fn transpose_solves_adjoint_in_all_storage_modes() {
        // unsymmetric complex system (chain + one-way transconductances + a branch)
        for n in [20, 60, 600] {
            let mut a = vec![];
            for i in 1..n - 1 {
                let g = C64::new(1e-3 * i as f64, 1e-4);
                a.extend([(i, i, g), (i + 1, i + 1, g), (i, i + 1, -g), (i + 1, i, -g)]);
                if i + 3 < n - 1 {
                    a.push((i, i + 3, C64::new(2e-4, 0.0)));
                }
            }
            a.extend([
                (1, n, C64::from(1.0)),
                (n, 1, C64::from(1.0)),
                (n - 1, n - 1, C64::from(1e-3)),
            ]);
            let (mut m, mut t) = (Matrix::<C64>::new(n), Matrix::<C64>::new(n));
            for _ in 0..2 {
                m.clear();
                for &(i, j, v) in &a {
                    m.add(i, j, v);
                }
                m.transpose_into(&mut t);
                let mut c = vec![C64::default(); n + 1];
                (c[n / 2], c[n - 1]) = (C64::from(1.0), C64::new(0.0, -2.0));
                for (i, &v) in c.iter().enumerate() {
                    t.rhs(i, v);
                }
                let mut y = vec![C64::default(); n + 1];
                t.solve(&mut y).unwrap();
                for &(i, j, v) in &a {
                    c[j] -= v * y[i];
                }
                assert!(c[1..].iter().all(|r| r.norm() < 1e-12), "n={n}");
            }
        }
    }
}
