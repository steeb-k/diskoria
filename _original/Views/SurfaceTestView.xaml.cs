using System.Windows;
using System.Windows.Controls;
using System.Windows.Input;
using JFStorageTester.ViewModels;

namespace JFStorageTester.Views;

public partial class SurfaceTestView : UserControl
{
    public SurfaceTestView()
    {
        InitializeComponent();
        Loaded += OnLoaded;
    }
    
    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        if (Application.Current?.MainWindow?.DataContext is MainViewModel mainVm)
        {
            DataContext = mainVm.SurfaceTestViewModel;
        }
    }

    private void ResultOverlay_Click(object sender, MouseButtonEventArgs e)
    {
        if (DataContext is SurfaceTestViewModel vm)
        {
            vm.ShowResultOverlay = false;
        }
    }
}
