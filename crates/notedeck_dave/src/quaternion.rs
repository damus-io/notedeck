use crate::Vec3;

// A simple quaternion implementation
pub struct Quaternion {
    pub x: f32,
    pub y: f32,
    pub z: f32,
    pub w: f32,
}

impl Quaternion {
    // Create identity quaternion
    pub fn identity() -> Self {
        Self {
            x: 0.0,
            y: 0.0,
            z: 0.0,
            w: 1.0,
        }
    }

    // Create from axis-angle representation
    pub fn from_axis_angle(axis: &Vec3, angle: f32) -> Self {
        let half_angle = angle * 0.5;
        let s = half_angle.sin();
        Self {
            x: axis.x * s,
            y: axis.y * s,
            z: axis.z * s,
            w: half_angle.cos(),
        }
    }

    // Multiply two quaternions (combines rotations)
    pub fn multiply(&self, other: &Self) -> Self {
        Self {
            x: self.w * other.x + self.x * other.w + self.y * other.z - self.z * other.y,
            y: self.w * other.y - self.x * other.z + self.y * other.w + self.z * other.x,
            z: self.w * other.z + self.x * other.y - self.y * other.x + self.z * other.w,
            w: self.w * other.w - self.x * other.x - self.y * other.y - self.z * other.z,
        }
    }

    // Convert quaternion to 4x4 matrix (for 3D transformation with homogeneous coordinates)
    pub fn to_matrix4(&self) -> [f32; 16] {
        // Normalize quaternion
        let magnitude =
            (self.x * self.x + self.y * self.y + self.z * self.z + self.w * self.w).sqrt();
        let x = self.x / magnitude;
        let y = self.y / magnitude;
        let z = self.z / magnitude;
        let w = self.w / magnitude;

        let x2 = x * x;
        let y2 = y * y;
        let z2 = z * z;
        let xy = x * y;
        let xz = x * z;
        let yz = y * z;
        let wx = w * x;
        let wy = w * y;
        let wz = w * z;

        // Row-major 3x3 rotation matrix components
        let m00 = 1.0 - 2.0 * (y2 + z2);
        let m01 = 2.0 * (xy - wz);
        let m02 = 2.0 * (xz + wy);

        let m10 = 2.0 * (xy + wz);
        let m11 = 1.0 - 2.0 * (x2 + z2);
        let m12 = 2.0 * (yz - wx);

        let m20 = 2.0 * (xz - wy);
        let m21 = 2.0 * (yz + wx);
        let m22 = 1.0 - 2.0 * (x2 + y2);

        // Convert 3x3 rotation matrix to 4x4 transformation matrix
        // Note: This is column-major for WGPU
        [
            m00, m10, m20, 0.0, m01, m11, m21, 0.0, m02, m12, m22, 0.0, 0.0, 0.0, 0.0, 1.0,
        ]
    }
}
