//! MNA matrix + LU solve, generic over real (DC/transient) and complex (AC).
//!
//! shortcut: dense storage with sparse-aware elimination (skips zero rows/cols).
//! Fast up to a few hundred unknowns; swap for a true sparse LU (KLU-style
//! symbolic reuse) for netlists with thousands of nodes.

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
        C64::new(self.re * o.re - self.im * o.im, self.re * o.im + self.im * o.re)
    }
}
impl Div for C64 {
    type Output = C64;
    fn div(self, o: C64) -> C64 {
        let d = o.re * o.re + o.im * o.im;
        C64::new((self.re * o.re + self.im * o.im) / d, (self.im * o.re - self.re * o.im) / d)
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

/// Square system of `n` unknowns. Index 0 is ground and is dropped by the
/// stamping helpers, so devices index unknowns 1..=n directly.
pub struct Matrix<T> {
    pub n: usize,
    a: Vec<T>,
    b: Vec<T>,
    nz: Vec<usize>,
}

impl<T: Scalar> Matrix<T> {
    pub fn new(n: usize) -> Self {
        Matrix { n, a: vec![T::default(); n * n], b: vec![T::default(); n], nz: Vec::with_capacity(n) }
    }
    pub fn clear(&mut self) {
        self.a.fill(T::default());
        self.b.fill(T::default());
    }
    #[inline]
    pub fn add(&mut self, r: usize, c: usize, v: T) {
        if r != 0 && c != 0 {
            self.a[(r - 1) * self.n + c - 1] += v;
        }
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
        self.add(a, a, g);
        self.add(b, b, g);
        self.add(a, b, -g);
        self.add(b, a, -g);
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
        self.add(a, p, g);
        self.add(a, n, -g);
        self.add(b, p, -g);
        self.add(b, n, g);
    }
    /// Branch unknown `k` carries current from a to b (voltage-source style).
    #[inline]
    pub fn branch(&mut self, a: usize, b: usize, k: usize) {
        self.add(a, k, T::from(1.0));
        self.add(b, k, T::from(-1.0));
        self.add(k, a, T::from(1.0));
        self.add(k, b, T::from(-1.0));
    }

    /// Solve in place; `x` has length n+1 and x[0] stays 0.
    /// On a singular matrix returns the unknown index that has no pivot.
    pub fn solve(&mut self, x: &mut [T]) -> Result<(), usize> {
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
}
