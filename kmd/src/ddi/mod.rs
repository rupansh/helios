//! WDDM DDI entry points, grouped by subsystem. `lib.rs` wires these into the
//! `DRIVER_INITIALIZATION_DATA` table.

mod add_device;
mod build_paging_buffer;
mod create_allocation;
mod escape;
mod interrupt;
mod query_adapter_info;
mod start_device;
mod submit_command;

pub use add_device::dxgkddi_add_device;
pub use build_paging_buffer::{
    dxgkddi_build_paging_buffer, dxgkddi_get_root_page_table_size, dxgkddi_set_root_page_table,
};
pub use create_allocation::{
    dxgkddi_close_allocation, dxgkddi_create_allocation, dxgkddi_describe_allocation,
    dxgkddi_destroy_allocation, dxgkddi_get_standard_allocation_driver_data,
    dxgkddi_open_allocation,
};
pub use escape::dxgkddi_escape;
pub use interrupt::{dxgkddi_control_interrupt, dxgkddi_dpc_routine, dxgkddi_interrupt_routine};
pub use query_adapter_info::{dxgkddi_get_node_metadata, dxgkddi_query_adapter_info};
pub use start_device::{
    dxgkddi_control_etw_logging, dxgkddi_dispatch_io_request, dxgkddi_notify_acpi_event,
    dxgkddi_query_child_relations, dxgkddi_query_child_status, dxgkddi_query_device_descriptor,
    dxgkddi_query_interface, dxgkddi_remove_device, dxgkddi_reset_device, dxgkddi_set_power_state,
    dxgkddi_start_device, dxgkddi_stop_device, dxgkddi_unload,
};
pub use submit_command::{
    dxgkddi_collect_dbg_info, dxgkddi_patch, dxgkddi_preempt_command, dxgkddi_query_current_fence,
    dxgkddi_render, dxgkddi_render_km, dxgkddi_reset_from_timeout, dxgkddi_restart_from_timeout,
    dxgkddi_submit_command, dxgkddi_submit_command_virtual,
};
