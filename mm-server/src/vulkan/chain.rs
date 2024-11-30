// Copyright 2024 Colin Marc <hi@colinmarc.com>
//
// SPDX-License-Identifier: BUSL-1.1

/// Used to construct a pinned chain of vulkan structures.
///
/// Ash provides the builder pattern for generating temporary input structs on
/// the stack, but it doesn't work well with structs stored on the heap for
/// re-use. Part of the reason for that is that the `p_next` pointer mechanism
/// is out of scope of the borrow checker.
///
/// If we want to store a chain of structs in a `Box`, we should also `Pin` it,
/// since the holding struct is effectively self-referential. This macro handles
/// the boilerplate for that, by:
///
///  - Generating a constructor for the struct that returns `Pin<Box<Self>>`
///  - Generating `AsRef<T>` and `AsMut<T>` for the first struct in the chain
///  - Generating `with_foo` methods that allow you to replace one struct in the
///    chain (presumably using the builder pattern).
///
/// Besides letting us reuse allocations for heavy structs, this also achieves a
/// level of polymorphism, since calling code can take an `impl AsRef<T>` where
/// `T` is the first struct, and generalize over the remaining chain.
///
/// Note that the chain is always created and maintained in declaration order,
/// with the first field being the "head" and the head's `p_next` pointer
/// pointing to the second field, and so on.
macro_rules! vk_chain {
    (
        $(#[$meta:meta])*
        $vis:vis struct $Chain:ident <$lifetime:lifetime> {
            $(#[$head_meta:meta])*
            pub $HeadName:ident: $HeadStruct:ty,
            $(
                $(#[$field_meta:meta])*
                pub $Name:ident: $Struct:ty,
            )+
        }
    ) => {
        paste::paste! {
            $(#[$meta])*
            $vis struct [<$Chain Inner>] <$lifetime> {
                $(#[$head_meta])*
                pub $HeadName: $HeadStruct,
                $(
                    $(#[$field_meta])*
                    pub $Name: $Struct,
                )*
            }

            $vis struct $Chain(std::pin::Pin<Box<[<$Chain Inner>] <'static> >>);

            unsafe impl Send for $Chain {}

            #[allow(dead_code)]
            impl $Chain {
                pub fn new<$lifetime: 'static>($HeadName: $HeadStruct, $($Name: $Struct,)*) -> Self {
                    let mut ch = Box::pin([<$Chain Inner>] {
                        $HeadName,
                        $($Name,)*
                    });

                    __set_p_next!(ch, $HeadName, $($Name),*);
                    Self(ch)
                }

                $(
                    #[doc = "Replaces the `" $Name "` field with the new (or modified) struct returned by `f`. Maintains the `p_next` chain."]
                    pub fn [<with_ $Name>]<$lifetime: 'static, F>(&mut self, f: F)
                    where
                        F: FnOnce($Struct) -> $Struct,
                    {
                        let p_next = self.0.$Name.p_next;
                        self.0.$Name = f(self.$Name);
                        self.0.$Name.p_next = p_next;
                    }
                )*
            }

            impl Default for $Chain {
                fn default() -> Self {
                    Self::new(__replace_expr!($HeadStruct Default::default()), $(__replace_expr!($Struct Default::default()),)*)
                }
            }

            impl std::ops::Deref for $Chain {
                type Target = [<$Chain Inner>]<'static>;

                fn deref(&self) -> &Self::Target {
                    std::pin::Pin::deref(&self.0)
                }
            }
        }

        impl<$lifetime: 'static> AsRef<$HeadStruct> for $Chain {
            fn as_ref(&self) -> &$HeadStruct {
                &self.0.as_ref().get_ref().$HeadName
            }
        }

        impl<$lifetime: 'static> AsMut<$HeadStruct> for $Chain {
            fn as_mut(&mut self) -> &mut $HeadStruct {
                &mut self.0.as_mut().get_mut().$HeadName
            }
        }
    };
}

macro_rules! __set_p_next(
    ($target:ident, $head:ident, $next:ident) => {
        $target.$head.p_next = <*mut _>::cast(&mut $target.$next);
    };
    ($target:ident, $head:ident, $next:ident, $($tail:ident),+) => {
        $target.$head.p_next = <*mut _>::cast(&mut $target.$next);
        __set_p_next!($target, $next, $($tail),+);
    };
);

macro_rules! __replace_expr {
    ($_t:tt $sub:expr) => {
        $sub
    };
}

pub(crate) use __replace_expr;
pub(crate) use __set_p_next;
pub(crate) use vk_chain;

#[cfg(test)]
mod tests {
    use ash::vk;

    #[test]
    fn test_chain() {
        vk_chain! {
            pub struct H264EncodeProfile<'a> {
                pub profile: vk::VideoProfileInfoKHR<'a>,
                pub encode_usage_info: vk::VideoEncodeUsageInfoKHR<'a>,
                pub h264_profile: vk::VideoEncodeH264ProfileInfoKHR<'a>,
            }
        }

        let mut chain = H264EncodeProfile::new(
            vk::VideoProfileInfoKHR::default(),
            vk::VideoEncodeUsageInfoKHR::default(),
            vk::VideoEncodeH264ProfileInfoKHR::default(),
        );

        chain.with_encode_usage_info(|info| {
            info.video_usage_hints(vk::VideoEncodeUsageFlagsKHR::STREAMING)
        });

        assert_eq!(
            chain.encode_usage_info.video_usage_hints,
            vk::VideoEncodeUsageFlagsKHR::STREAMING
        );
    }
}
