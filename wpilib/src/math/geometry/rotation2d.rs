use crate::math::units::angle::Radian;

use nalgebra::ComplexField;

#[derive(Copy, Clone, Debug, PartialEq)]
pub struct Rotation2d {
    pub value: Radian,
    pub sin: f64,
    pub cos: f64,
}
impl Rotation2d {
    pub fn new() -> Self {
        Self {
            value: 0.0.into(),
            sin: 0.0,
            cos: 1.0,
        }
    }
    pub fn new_angle(angle: impl Into<Radian>) -> Self {
        let value: Radian = angle.into();
        Self {
            value,
            sin: value.sin().into(),
            cos: value.cos().into(),
        }
    }
    pub fn new_xy(x: f64, y: f64) -> Self {
        let magnitude = x.hypot(y);
        let sin;
        let cos;
        if magnitude > 1e-6 {
            sin = y / magnitude;
            cos = x / magnitude;
        } else {
            sin = 0.0;
            cos = 1.0;
        }
        let value = sin.atan2(cos);
        Self {
            value: value.into(),
            sin,
            cos,
        }
    }
    
    
    pub fn plus(&self, other: &Self) -> Self {
        Self::rotate_by(&self, other)
    }
    pub fn minus(&self, other: &Self) -> Self {
        Self::rotate_by(&self, &other.unary_minus())
    }
    pub fn unary_minus(&self) -> Self {
        Self::new_angle(-self.value)
    }
    pub fn times(&self, scalar: f64) -> Self {
        Self::new_angle(f64::from(self.value) * scalar)
    }
    pub fn div(&self, scalar: f64) -> Self {
        Self::times(&self, (1.0 / scalar))
    }
    pub fn rotate_by(&self, other: &Self) -> Self {
        Self::new_xy(
            self.cos * other.cos - self.sin * other.sin,
            self.cos * other.sin - self.sin * other.cos
        )
    }


    pub fn get_tan(&self) -> f64 {
        self.sin / self.cos
    }
}