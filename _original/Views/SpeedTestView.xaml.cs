using System.Windows;
using System.Windows.Controls;
using JFStorageTester.ViewModels;

namespace JFStorageTester.Views;

public partial class SpeedTestView : UserControl
{
    public SpeedTestView()
    {
        InitializeComponent();
        Loaded += OnLoaded;
    }
    
    private void OnLoaded(object sender, RoutedEventArgs e)
    {
        if (Application.Current?.MainWindow?.DataContext is MainViewModel mainVm)
        {
            DataContext = mainVm.SpeedTestViewModel;
        }
    }
}
