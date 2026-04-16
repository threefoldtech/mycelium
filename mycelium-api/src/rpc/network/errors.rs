use std::fmt;

#[derive(Debug)]
pub enum NetworkError {
    UnsupportedPlatform,
    PermissionDenied,
    InvalidArgument(String),
    BridgeNotFound,
    BridgeNotEmpty,
    InvalidBridgeName,
    InterfaceNotFound,
    InvalidInterfaceType,
    AddressOutOfRange,
    InvalidAddressFamily,
    AddressAlreadyExists,
    AddressNotFound,
    ListenerActivationFailed(String),
    Internal(String),
}

impl NetworkError {
    pub fn code(&self) -> i32 {
        match self {
            Self::UnsupportedPlatform => 1001,
            Self::PermissionDenied => 1002,
            Self::InvalidArgument(_) => 1003,
            Self::BridgeNotFound => 1101,
            Self::BridgeNotEmpty => 1102,
            Self::InvalidBridgeName => 1103,
            Self::InterfaceNotFound => 1201,
            Self::InvalidInterfaceType => 1202,
            Self::AddressOutOfRange => 1203,
            Self::InvalidAddressFamily => 1204,
            Self::AddressAlreadyExists => 1205,
            Self::AddressNotFound => 1206,
            Self::ListenerActivationFailed(_) => 1301,
            Self::Internal(_) => 1900,
        }
    }

    pub fn message(&self) -> &'static str {
        match self {
            Self::UnsupportedPlatform => "unsupported_platform",
            Self::PermissionDenied => "permission_denied",
            Self::InvalidArgument(_) => "invalid_argument",
            Self::BridgeNotFound => "bridge_not_found",
            Self::BridgeNotEmpty => "bridge_not_empty",
            Self::InvalidBridgeName => "invalid_bridge_name",
            Self::InterfaceNotFound => "interface_not_found",
            Self::InvalidInterfaceType => "invalid_interface_type",
            Self::AddressOutOfRange => "address_out_of_range",
            Self::InvalidAddressFamily => "invalid_address_family",
            Self::AddressAlreadyExists => "address_already_exists",
            Self::AddressNotFound => "address_not_found",
            Self::ListenerActivationFailed(_) => "listener_activation_failed",
            Self::Internal(_) => "internal_error",
        }
    }
}

impl fmt::Display for NetworkError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(d)
            | Self::ListenerActivationFailed(d)
            | Self::Internal(d) => write!(f, "{}: {d}", self.message()),
            _ => f.write_str(self.message()),
        }
    }
}

impl std::error::Error for NetworkError {}
