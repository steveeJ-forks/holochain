/// commit the creation of an entry
/// accepts any expression that evaluates to something that implements TryInto<SerializedBytes> and
/// Into<EntryDefId>, so the defaults from the `#[hdk_entry( .. )]` and `entry_def!()` macros
/// make any struct/enum into a committable entry.
///
/// e.g.
/// ```ignore
/// #[hdk_entry(id = "foo")]
/// pub struct Foo(u32);
/// commit_entry!(Foo(50))?;
/// ```
///
/// @see get! and get_details! for more information on CRUD
///
/// @todo do we need/want to expose an alternative pattern to match to allow manually setting the
/// entry id rather than calling .into()?
#[macro_export]
macro_rules! create_entry {
    ( $input:expr ) => {{
        $crate::prelude::host_externs!(__create);

        let try_sb = $crate::prelude::SerializedBytes::try_from($input);
        match try_sb {
            Ok(sb) => create!($input, $crate::prelude::Entry::App(sb.try_into()?)),
            Err(e) => Err(e),
        }
    }};
}
